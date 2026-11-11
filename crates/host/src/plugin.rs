//! [`HostedPlugin`]: a VST3 or CLAP plugin behind `aitrack_dsp::Processor`.
//!
//! # Threading
//!
//! A hosted plugin has two halves:
//!
//! - the **audio half**, owned by [`HostedPlugin`] (`Send`, moves to the
//!   audio thread): `process`, `set_param`, `get_param`, `reset`;
//! - the **main half** (VST3 controller and view, CLAP `[main-thread]`
//!   functions, the editor window), confined to the thread that loaded the
//!   plugin and reached through [`PluginController`], a cheap `Clone + Send +
//!   Sync` handle that checks the calling thread at runtime.
//!
//! `prepare` needs both halves (it re-activates the plugin), so it must run
//! on the main thread while the plugin is not processing, which matches the
//! `Processor::prepare` contract. The DAW calls [`PluginController::idle`]
//! from its UI tick (~30–60 Hz) to deliver main-thread callbacks, resize and
//! close editors, and sync parameter values to the plugin's controller.
//!
//! # Crash containment
//!
//! Every call into the plugin runs under `catch_unwind`. A panic (from a
//! Rust plugin, or from our own glue) marks the plugin as crashed; from then
//! on it outputs silence and ignores calls. Non-finite output samples are
//! replaced with zeros. Hard faults (access violations) cannot be caught in
//! process: that is what the out-of-process [`sandbox`](crate::sandbox) is for.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use aitrack_dsp::{Category, PluginInfo, ProcessContext, Processor, Transport};
use aitrack_model::PluginFormat;

use crate::error::{HostError, Result};
use crate::info::HostedPluginInfo;
use crate::params::{ParamEdit, ParamTable, PendingParams};
use crate::strings::intern;
use crate::thread::MainThreadCell;

/// State shared between the two halves (lock-free).
pub(crate) struct SharedState {
    pub hosted: HostedPluginInfo,
    pub params: ParamTable,
    pub latency: AtomicU32,
    pub tail: AtomicU32,
    pub crashed: AtomicBool,
    /// The plugin asked to be re-activated (CLAP `request_restart`, VST3
    /// `kReloadComponent`/`kLatencyChanged`, bus changes): the engine should
    /// call `prepare` again from the main thread.
    pub needs_restart: AtomicBool,
    pub latency_changed: AtomicBool,
    pub state_dirty: AtomicBool,
}

pub(crate) struct PluginShared {
    pub state: SharedState,
    pub main: MainThreadCell<MainSide>,
}

pub(crate) enum MainSide {
    Vst3(Box<crate::vst3::Vst3Main>),
    Clap(Box<crate::clap::ClapMain>),
}

pub(crate) enum AudioSide {
    Vst3(Box<crate::vst3::Vst3Audio>),
    Clap(Box<crate::clap::ClapAudio>),
}

/// What a format back end hands over after instantiating a plugin.
pub(crate) struct Loaded {
    pub main: MainSide,
    pub audio: AudioSide,
    pub info: HostedPluginInfo,
    pub params: Vec<aitrack_dsp::ParamInfo>,
    /// Initial normalised values `(id, value)`.
    pub values: Vec<(u32, f32)>,
    pub edits: rtrb::Consumer<ParamEdit>,
}

impl MainSide {
    fn prepare(&mut self, audio: &mut AudioSide, state: &SharedState, sample_rate: f64, max_frames: usize) -> Result<()> {
        match (self, audio) {
            (MainSide::Vst3(m), AudioSide::Vst3(a)) => m.prepare(a, state, sample_rate, max_frames),
            (MainSide::Clap(m), AudioSide::Clap(a)) => m.prepare(a, state, sample_rate, max_frames),
            _ => Err(HostError::Activate("mismatched back ends".into())),
        }
    }

    fn deactivate(&mut self, audio: &mut AudioSide) {
        match (self, audio) {
            (MainSide::Vst3(m), AudioSide::Vst3(a)) => m.deactivate(a),
            (MainSide::Clap(m), AudioSide::Clap(a)) => m.deactivate(a),
            _ => {}
        }
    }

    fn save_state(&mut self) -> Result<Vec<u8>> {
        match self {
            MainSide::Vst3(m) => m.save_state(),
            MainSide::Clap(m) => m.save_state(),
        }
    }

    fn load_state(&mut self, state: &SharedState, data: &[u8]) -> Result<()> {
        match self {
            MainSide::Vst3(m) => m.load_state(state, data),
            MainSide::Clap(m) => m.load_state(state, data),
        }
    }

    fn has_editor(&mut self) -> bool {
        match self {
            MainSide::Vst3(m) => m.has_editor(),
            MainSide::Clap(m) => m.has_editor(),
        }
    }

    fn open_editor(&mut self, title: &str) -> Result<()> {
        match self {
            MainSide::Vst3(m) => m.open_editor(title),
            MainSide::Clap(m) => m.open_editor(title),
        }
    }

    fn close_editor(&mut self) {
        match self {
            MainSide::Vst3(m) => m.close_editor(),
            MainSide::Clap(m) => m.close_editor(),
        }
    }

    fn is_editor_open(&self) -> bool {
        match self {
            MainSide::Vst3(m) => m.is_editor_open(),
            MainSide::Clap(m) => m.is_editor_open(),
        }
    }

    fn idle(&mut self, state: &SharedState) {
        match self {
            MainSide::Vst3(m) => m.idle(state),
            MainSide::Clap(m) => m.idle(state),
        }
    }

    fn param_text(&mut self, state: &SharedState, id: u32, normalized: f32) -> Option<String> {
        match self {
            MainSide::Vst3(m) => m.param_text(id, normalized),
            MainSide::Clap(m) => m.param_text(state, id, normalized),
        }
    }
}

impl AudioSide {
    fn process(&mut self, ctx: &mut ProcessContext<'_>, state: &SharedState, pending: &mut PendingParams) -> bool {
        match self {
            AudioSide::Vst3(a) => a.process(ctx, state, pending),
            AudioSide::Clap(a) => a.process(ctx, state, pending),
        }
    }

    fn reset(&mut self) {
        match self {
            AudioSide::Vst3(a) => a.reset(),
            AudioSide::Clap(a) => a.reset(),
        }
    }
}

/// A third-party plugin instance hosted in this process.
pub struct HostedPlugin {
    info: PluginInfo,
    shared: Arc<PluginShared>,
    audio: Option<AudioSide>,
    pending: PendingParams,
    edits: Option<rtrb::Consumer<ParamEdit>>,
    prepared: bool,
    crashed: bool,
}

impl HostedPlugin {
    /// Instantiate the plugin described by `info` (from the scan cache or a
    /// project). Call on the main thread; that thread becomes the plugin's
    /// main thread.
    pub fn load(info: &HostedPluginInfo) -> Result<Self> {
        let loaded = catch_unwind(AssertUnwindSafe(|| match info.format {
            PluginFormat::Vst3 => crate::vst3::load(info),
            PluginFormat::Clap => crate::clap::load(info),
        }))
        .map_err(|_| HostError::Instantiate("plugin panicked while loading".into()))??;
        Ok(Self::from_loaded(loaded))
    }

    /// Load the first (or the `uid`-matching) plugin in a module file.
    pub fn load_path(format: PluginFormat, path: &std::path::Path, uid: Option<&str>) -> Result<Self> {
        let infos = match format {
            PluginFormat::Vst3 => crate::vst3::enumerate(path)?,
            PluginFormat::Clap => crate::clap::enumerate(path)?,
        };
        let info = match uid {
            Some(uid) => infos.into_iter().find(|i| i.uid == uid).ok_or_else(|| HostError::NotFound(uid.into()))?,
            None => infos.into_iter().next().ok_or_else(|| HostError::NotFound(path.display().to_string()))?,
        };
        Self::load(&info)
    }

    fn from_loaded(loaded: Loaded) -> Self {
        let Loaded { main, audio, info: hosted, params, values, edits } = loaded;
        let info = PluginInfo {
            id: intern(&hosted.stable_id()),
            name: intern(&hosted.name),
            vendor: intern(&hosted.vendor),
            category: if hosted.is_instrument { Category::Instrument } else { Category::Effect },
            tags: &[],
            params: params.clone(),
            inputs: hosted.inputs,
            outputs: hosted.outputs,
            has_sidechain: false,
        };
        let table = ParamTable::new(params);
        for (id, v) in values {
            table.set(id, v);
        }
        let pending = PendingParams::new(table.len());
        let shared = Arc::new(PluginShared {
            state: SharedState {
                hosted,
                params: table,
                latency: AtomicU32::new(0),
                tail: AtomicU32::new(0),
                crashed: AtomicBool::new(false),
                needs_restart: AtomicBool::new(false),
                latency_changed: AtomicBool::new(false),
                state_dirty: AtomicBool::new(false),
            },
            main: MainThreadCell::new(main),
        });
        Self { info, shared, audio: Some(audio), pending, edits: Some(edits), prepared: false, crashed: false }
    }

    /// Main-thread handle (editor, state, idle). Cheap to clone.
    pub fn controller(&self) -> PluginController {
        PluginController { shared: self.shared.clone() }
    }

    pub fn hosted_info(&self) -> &HostedPluginInfo {
        &self.shared.state.hosted
    }

    /// The lock-free queue of edits made in the plugin's GUI, for automation
    /// recording and undo. Can be taken once.
    pub fn take_param_edits(&mut self) -> Option<rtrb::Consumer<ParamEdit>> {
        self.edits.take()
    }

    pub fn is_crashed(&self) -> bool {
        self.crashed || self.shared.state.crashed.load(Ordering::Relaxed)
    }

    pub fn is_prepared(&self) -> bool {
        self.prepared
    }

    /// The plugin asked to be re-activated; call `prepare` again from the
    /// main thread (clears the flag).
    pub fn take_needs_restart(&self) -> bool {
        self.shared.state.needs_restart.swap(false, Ordering::AcqRel)
    }

    /// Main thread: open the plugin GUI in its own window.
    pub fn open_editor(&mut self) -> Result<()> {
        self.controller().open_editor()
    }

    /// Main thread: close the plugin GUI.
    pub fn close_editor(&mut self) {
        self.controller().close_editor()
    }

    fn mark_crashed(&mut self, what: &str) {
        log::error!("plugin '{}' panicked in {what}; disabled", self.info.name);
        self.crashed = true;
        self.shared.state.crashed.store(true, Ordering::Relaxed);
    }
}

impl Processor for HostedPlugin {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, sample_rate: f64, max_frames: usize) {
        if self.is_crashed() {
            return;
        }
        let Some(audio) = self.audio.as_mut() else { return };
        let shared = &self.shared;
        let result = catch_unwind(AssertUnwindSafe(|| {
            shared.main.with(|main| main.prepare(audio, &shared.state, sample_rate, max_frames))
        }));
        match result {
            Ok(Ok(Ok(()))) => self.prepared = true,
            Ok(Ok(Err(e))) => {
                log::error!("plugin '{}' failed to activate: {e}", self.info.name);
                self.prepared = false;
            }
            Ok(Err(e)) => {
                log::error!("HostedPlugin::prepare: {e}");
                self.prepared = false;
            }
            Err(_) => self.mark_crashed("prepare"),
        }
    }

    fn reset(&mut self) {
        if self.is_crashed() || !self.prepared {
            return;
        }
        let Some(audio) = self.audio.as_mut() else { return };
        if catch_unwind(AssertUnwindSafe(|| audio.reset())).is_err() {
            self.mark_crashed("reset");
        }
    }

    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        if self.is_crashed() || !self.prepared {
            ctx.buffer.clear();
            return;
        }
        let Some(audio) = self.audio.as_mut() else {
            ctx.buffer.clear();
            return;
        };
        let state = &self.shared.state;
        let pending = &mut self.pending;
        match catch_unwind(AssertUnwindSafe(|| audio.process(ctx, state, pending))) {
            Ok(true) => {
                crate::io::sanitize(ctx.buffer);
            }
            Ok(false) => ctx.buffer.clear(),
            Err(_) => {
                ctx.buffer.clear();
                self.mark_crashed("process");
            }
        }
    }

    fn set_param(&mut self, id: u32, normalized: f32) {
        let params = &self.shared.state.params;
        if let Some(index) = params.index_of(id) {
            let v = normalized.clamp(0.0, 1.0);
            params.set(id, v);
            self.pending.push(index, v);
        }
    }

    fn get_param(&self, id: u32) -> f32 {
        self.shared.state.params.get(id)
    }

    fn latency(&self) -> u32 {
        self.shared.state.latency.load(Ordering::Relaxed)
    }

    fn tail(&self) -> u32 {
        self.shared.state.tail.load(Ordering::Relaxed)
    }

    /// Main thread only (VST3 `getState`, CLAP `state.save`); returns an
    /// empty vector elsewhere or on failure.
    fn save_state(&self) -> Vec<u8> {
        match self.controller().save_state() {
            Ok(s) => s,
            Err(e) => {
                log::error!("save_state for '{}': {e}", self.info.name);
                Vec::new()
            }
        }
    }

    /// Main thread only (VST3 `setState`, CLAP `state.load`).
    fn load_state(&mut self, data: &[u8]) {
        if let Err(e) = self.controller().load_state(data) {
            log::error!("load_state for '{}': {e}", self.info.name);
        }
    }
}

impl Drop for HostedPlugin {
    fn drop(&mut self) {
        let Some(mut audio) = self.audio.take() else { return };
        if self.shared.main.is_owner() {
            let shared = &self.shared;
            let _ = catch_unwind(AssertUnwindSafe(|| {
                let _ = shared.main.with(|main| {
                    main.close_editor();
                    main.deactivate(&mut audio);
                });
            }));
            drop(audio);
        } else {
            log::error!("HostedPlugin '{}' dropped off the main thread; leaking it", self.info.name);
            std::mem::forget(audio);
        }
    }
}

/// Main-thread handle to a hosted plugin. Every method checks that it runs
/// on the thread that loaded the plugin and returns
/// [`HostError::WrongThread`] (or does nothing) otherwise.
#[derive(Clone)]
pub struct PluginController {
    shared: Arc<PluginShared>,
}

impl PluginController {
    pub fn info(&self) -> &HostedPluginInfo {
        &self.shared.state.hosted
    }

    pub fn params(&self) -> &[aitrack_dsp::ParamInfo] {
        self.shared.state.params.infos()
    }

    /// Current normalised value (thread-safe).
    pub fn param_value(&self, id: u32) -> f32 {
        self.shared.state.params.get(id)
    }

    /// The plugin's own display string for a value ("-6.0 dB").
    pub fn param_text(&self, id: u32, normalized: f32) -> Option<String> {
        let state = &self.shared.state;
        self.call(|m| m.param_text(state, id, normalized)).ok().flatten()
    }

    pub fn latency(&self) -> u32 {
        self.shared.state.latency.load(Ordering::Relaxed)
    }

    /// Latency changed since the last call (re-plan delay compensation).
    pub fn take_latency_changed(&self) -> bool {
        self.shared.state.latency_changed.swap(false, Ordering::AcqRel)
    }

    /// The plugin reported unsaved changes since the last call.
    pub fn take_state_dirty(&self) -> bool {
        self.shared.state.state_dirty.swap(false, Ordering::AcqRel)
    }

    pub fn needs_restart(&self) -> bool {
        self.shared.state.needs_restart.load(Ordering::Acquire)
    }

    pub fn is_crashed(&self) -> bool {
        self.shared.state.crashed.load(Ordering::Relaxed)
    }

    pub fn has_editor(&self) -> bool {
        self.call(|m| m.has_editor()).unwrap_or(false)
    }

    pub fn open_editor(&self) -> Result<()> {
        let title = format!("{} — {}", self.shared.state.hosted.name, self.shared.state.hosted.vendor);
        self.call(|m| if m.is_editor_open() { Ok(()) } else { m.open_editor(&title) })?
    }

    pub fn close_editor(&self) {
        let _ = self.call(|m| m.close_editor());
    }

    pub fn is_editor_open(&self) -> bool {
        self.call(|m| m.is_editor_open()).unwrap_or(false)
    }

    /// Main-thread housekeeping; call from the UI tick.
    pub fn idle(&self) {
        let state = &self.shared.state;
        let _ = self.call(|m| m.idle(state));
    }

    pub fn save_state(&self) -> Result<Vec<u8>> {
        self.call(|m| m.save_state())?
    }

    pub fn load_state(&self, data: &[u8]) -> Result<()> {
        let state = &self.shared.state;
        self.call(|m| m.load_state(state, data))?
    }

    fn call<R>(&self, f: impl FnOnce(&mut MainSide) -> R) -> Result<R> {
        if self.is_crashed() {
            return Err(HostError::Crashed);
        }
        let shared = &self.shared;
        match catch_unwind(AssertUnwindSafe(|| shared.main.with(f))) {
            Ok(r) => r,
            Err(_) => {
                log::error!("plugin '{}' panicked on the main thread; disabled", shared.state.hosted.name);
                shared.state.crashed.store(true, Ordering::Relaxed);
                Err(HostError::Crashed)
            }
        }
    }
}

/// Start of the bar containing the block start, in quarter notes.
pub(crate) fn bar_start_beats(t: &Transport) -> f64 {
    let (num, den) = (t.time_sig.0.max(1) as f64, t.time_sig.1.max(1) as f64);
    let quarters_per_bar = num * 4.0 / den;
    (t.position_beats / quarters_per_bar).floor() * quarters_per_bar
}

pub(crate) fn bar_number(t: &Transport) -> i32 {
    let (num, den) = (t.time_sig.0.max(1) as f64, t.time_sig.1.max(1) as f64);
    (t.position_beats / (num * 4.0 / den)).floor() as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bar_math() {
        let t = Transport { position_beats: 9.5, time_sig: (4, 4), ..Default::default() };
        assert_eq!(bar_start_beats(&t), 8.0);
        assert_eq!(bar_number(&t), 2);
        let t = Transport { position_beats: 7.0, time_sig: (6, 8), ..Default::default() };
        assert_eq!(bar_start_beats(&t), 6.0);
    }
}
