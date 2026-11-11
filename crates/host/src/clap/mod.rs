//! CLAP hosting on top of `clack-host` / `clack-extensions`.
//!
//! Host extensions provided to plugins: `log`, `params`, `state`,
//! `latency`, `gui`, `audio-ports`, `note-ports`, `timer-support`, `tail`,
//! plus the core `request_restart` / `request_process` / `request_callback`.
//! Callbacks only set flags; the work happens on the main thread in
//! [`ClapMain::idle`] or at the next `prepare`.

mod instance;

pub(crate) use instance::{ClapAudio, ClapMain, load};

use std::cell::RefCell;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use clack_extensions::audio_ports::{AudioPortRescanFlags, HostAudioPorts, HostAudioPortsImpl};
use clack_extensions::gui::{GuiSize, HostGui, HostGuiImpl};
use clack_extensions::latency::{HostLatency, HostLatencyImpl};
use clack_extensions::log::{HostLog, HostLogImpl, LogSeverity};
use clack_extensions::note_ports::{HostNotePorts, HostNotePortsImpl, NoteDialects, NotePortRescanFlags};
use clack_extensions::params::{HostParams, HostParamsImplMainThread, HostParamsImplShared, ParamClearFlags, ParamRescanFlags};
use clack_extensions::state::{HostState, HostStateImpl};
use clack_extensions::tail::{HostTail, HostTailImpl};
use clack_extensions::timer::{HostTimer, HostTimerImpl, TimerId};
use clack_host::prelude::*;

use crate::error::{HostError as Error, Result};
use crate::info::HostedPluginInfo;
use aitrack_model::PluginFormat;

/// Flags raised by plugin → host callbacks (any thread).
#[derive(Default)]
pub(crate) struct ClapFlags {
    pub restart: AtomicBool,
    pub process: AtomicBool,
    pub callback: AtomicBool,
    pub flush: AtomicBool,
    /// `w << 32 | h`, valid when `resize` is set.
    pub resize_size: AtomicU64,
    pub resize: AtomicBool,
    pub gui_closed: AtomicBool,
    pub latency_changed: AtomicBool,
    pub params_rescan: AtomicBool,
    pub state_dirty: AtomicBool,
    pub tail_changed: AtomicBool,
}

pub(crate) struct ClapHost;

impl HostHandlers for ClapHost {
    type Shared<'a> = ClapShared;
    type MainThread<'a> = ClapMainThread;
    type AudioProcessor<'a> = ClapAudioHandler;

    fn declare_extensions(builder: &mut HostExtensions<Self>, _shared: &Self::Shared<'_>) {
        builder
            .register::<HostLog>()
            .register::<HostParams>()
            .register::<HostState>()
            .register::<HostLatency>()
            .register::<HostGui>()
            .register::<HostAudioPorts>()
            .register::<HostNotePorts>()
            .register::<HostTimer>()
            .register::<HostTail>();
    }
}

pub(crate) struct ClapShared {
    pub flags: Arc<ClapFlags>,
}

impl SharedHandler<'_> for ClapShared {
    fn request_restart(&self) {
        self.flags.restart.store(true, Ordering::Release);
    }
    fn request_process(&self) {
        self.flags.process.store(true, Ordering::Release);
    }
    fn request_callback(&self) {
        self.flags.callback.store(true, Ordering::Release);
    }
}

impl HostLogImpl for ClapShared {
    fn log(&self, severity: LogSeverity, message: &str) {
        let level = match severity {
            LogSeverity::Debug => log::Level::Debug,
            LogSeverity::Info => log::Level::Info,
            LogSeverity::Warning => log::Level::Warn,
            _ => log::Level::Error,
        };
        log::log!(target: "clap-plugin", level, "{message}");
    }
}

impl HostGuiImpl for ClapShared {
    fn resize_hints_changed(&self) {}

    fn request_resize(&self, new_size: GuiSize) -> std::result::Result<(), HostError> {
        self.flags.resize_size.store(((new_size.width as u64) << 32) | new_size.height as u64, Ordering::Release);
        self.flags.resize.store(true, Ordering::Release);
        Ok(())
    }

    fn request_show(&self) -> std::result::Result<(), HostError> {
        Ok(())
    }

    fn request_hide(&self) -> std::result::Result<(), HostError> {
        Ok(())
    }

    fn closed(&self, _was_destroyed: bool) {
        self.flags.gui_closed.store(true, Ordering::Release);
    }
}

impl HostParamsImplShared for ClapShared {
    fn request_flush(&self) {
        self.flags.flush.store(true, Ordering::Release);
    }
}

pub(crate) struct Timer {
    pub id: u32,
    pub period: Duration,
    pub next: Instant,
}

pub(crate) struct ClapMainThread {
    pub flags: Arc<ClapFlags>,
    pub timers: RefCell<Vec<Timer>>,
    next_timer: std::cell::Cell<u32>,
}

impl ClapMainThread {
    pub fn new(flags: Arc<ClapFlags>) -> Self {
        Self { flags, timers: RefCell::new(Vec::new()), next_timer: std::cell::Cell::new(1) }
    }
}

impl MainThreadHandler<'_> for ClapMainThread {}

impl HostParamsImplMainThread for ClapMainThread {
    fn rescan(&self, _flags: ParamRescanFlags) {
        self.flags.params_rescan.store(true, Ordering::Release);
    }
    fn clear(&self, _param_id: ClapId, _flags: ParamClearFlags) {}
}

impl HostStateImpl for ClapMainThread {
    fn mark_dirty(&self) {
        self.flags.state_dirty.store(true, Ordering::Release);
    }
}

impl HostLatencyImpl for ClapMainThread {
    fn changed(&self) {
        self.flags.latency_changed.store(true, Ordering::Release);
    }
}

impl HostAudioPortsImpl for ClapMainThread {
    fn is_rescan_flag_supported(&self, _flag: AudioPortRescanFlags) -> bool {
        // Every change is handled the same way: re-activate.
        true
    }
    fn rescan(&self, _flags: AudioPortRescanFlags) {
        self.flags.restart.store(true, Ordering::Release);
    }
}

impl HostNotePortsImpl for ClapMainThread {
    fn supported_dialects(&self) -> NoteDialects {
        NoteDialects::CLAP | NoteDialects::MIDI
    }
    fn rescan(&self, _flags: NotePortRescanFlags) {}
}

impl HostTimerImpl for ClapMainThread {
    fn register_timer(&self, period_ms: u32) -> std::result::Result<TimerId, HostError> {
        let id = self.next_timer.get();
        self.next_timer.set(id.wrapping_add(1).max(1));
        // At least ~30 Hz is honoured; faster timers are clamped to 10 ms.
        let period = Duration::from_millis(period_ms.max(10) as u64);
        self.timers.borrow_mut().push(Timer { id, period, next: Instant::now() + period });
        Ok(TimerId(id))
    }

    fn unregister_timer(&self, timer_id: TimerId) -> std::result::Result<(), HostError> {
        self.timers.borrow_mut().retain(|t| t.id != timer_id.0);
        Ok(())
    }
}

pub(crate) struct ClapAudioHandler {
    pub flags: Arc<ClapFlags>,
}

impl AudioProcessorHandler<'_> for ClapAudioHandler {}

impl HostTailImpl for ClapAudioHandler {
    fn changed(&mut self) {
        self.flags.tail_changed.store(true, Ordering::Release);
    }
}

pub(crate) fn host_info() -> Result<HostInfo> {
    HostInfo::new(crate::HOST_NAME, crate::HOST_VENDOR, crate::HOST_URL, crate::HOST_VERSION).map_err(|e| Error::Instantiate(e.to_string()))
}

pub(crate) fn load_entry(path: &Path) -> Result<PluginEntry> {
    // SAFETY: loading a plugin library runs its initialisers; that is the
    // inherent risk of hosting, which the scanner/sandbox processes contain.
    unsafe { PluginEntry::load(path) }.map_err(|e| Error::Load { path: path.to_path_buf(), reason: format!("{e:?}") })
}

/// List the plugins of a `.clap` file without instantiating them.
pub fn enumerate(path: &Path) -> Result<Vec<HostedPluginInfo>> {
    let entry = load_entry(path)?;
    let factory = entry.get_plugin_factory().ok_or_else(|| Error::Load { path: path.to_path_buf(), reason: "no plugin factory".into() })?;
    let mut out = Vec::new();
    for d in factory.plugin_descriptors() {
        let Some(id) = d.id() else { continue };
        let s = |c: Option<&std::ffi::CStr>| c.map(|c| c.to_string_lossy().into_owned()).unwrap_or_default();
        let features: Vec<String> = d.features().map(|f| f.to_string_lossy().into_owned()).collect();
        let is_instrument = features.iter().any(|f| f == "instrument");
        out.push(HostedPluginInfo {
            format: PluginFormat::Clap,
            uid: id.to_string_lossy().into_owned(),
            name: s(d.name()),
            vendor: s(d.vendor()),
            version: s(d.version()),
            category: features.join("|"),
            is_instrument,
            path: path.to_path_buf(),
            inputs: if is_instrument { 0 } else { 2 },
            outputs: 2,
            params: 0,
            probed: false,
        });
    }
    Ok(out)
}

/// Like [`enumerate`], then instantiates each plugin for its real port and
/// parameter counts. Used by the out-of-process scanner.
pub fn probe(path: &Path) -> Result<Vec<HostedPluginInfo>> {
    let mut infos = enumerate(path)?;
    for info in &mut infos {
        match load(info) {
            Ok(loaded) => {
                info.inputs = loaded.info.inputs;
                info.outputs = loaded.info.outputs;
                info.params = loaded.info.params;
                info.probed = true;
            }
            Err(e) => log::warn!("CLAP probe of {} failed: {e}", info.name),
        }
    }
    Ok(infos)
}
