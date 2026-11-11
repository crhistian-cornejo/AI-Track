//! VST3 and CLAP plugin hosting, scanning and caching for AI-Track.
//!
//! Layers, from the bottom up:
//!
//! - [`info`]: [`HostedPluginInfo`], the format-neutral description stored
//!   in the scan cache and in projects.
//! - [`vst3`] and [`clap`]: format back ends (module loading, host
//!   callbacks, buses, events, parameters, state, editors).
//! - [`plugin`]: [`HostedPlugin`], which wraps either back end and
//!   implements [`aitrack_dsp::Processor`], plus [`PluginController`], the
//!   main-thread handle (editor, state, idle housekeeping).
//! - [`editor`]: native top-level windows (HWND / NSWindow) that hold a
//!   plugin's GUI.
//! - [`scan`]: search paths, `moduleinfo.json`, out-of-process scanning with
//!   a timeout, the incremental cache and the persistent blocklist.
//! - [`sandbox`]: the out-of-process protocol (shared-memory audio, events,
//!   same-cycle handoff) and [`sandbox::SandboxedPlugin`], a `Processor`
//!   that drives a plugin living in the `aitrack-plugin-host` process.
//!
//! See `README.md` in this crate for the threading model, the sandbox
//! protocol and the macOS signing requirements.

pub mod clap;
pub mod editor;
pub mod error;
pub mod info;
mod io;
pub mod params;
pub mod plugin;
pub mod sandbox;
pub mod scan;
mod strings;
mod thread;
pub mod vst3;

pub use error::HostError;
pub use info::HostedPluginInfo;
pub use params::{ParamEdit, ParamEditKind};
pub use plugin::{HostedPlugin, PluginController};

pub use aitrack_model::PluginFormat;

/// Name reported to plugins (VST3 `IHostApplication::getName`, CLAP host info).
pub const HOST_NAME: &str = "AI-Track";
pub const HOST_VENDOR: &str = "AI-Track";
pub const HOST_URL: &str = "https://github.com/crhistian-cornejo/AI-Track";
pub const HOST_VERSION: &str = env!("CARGO_PKG_VERSION");
