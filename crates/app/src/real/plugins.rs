//! Processor factories and the plugin catalogue over `aitrack-stock` and
//! `aitrack-host`.
//!
//! * [`StockFactory`]: `DeviceKind::Stock` → `aitrack_stock::create`.
//! * [`HostedFactory`]: `DeviceKind::Plugin` → a VST3/CLAP plugin, in this
//!   process (loaded on the main thread, see [`MainThread`]) or in the
//!   `aitrack-plugin-host` sandbox when the setting is on. In process, an
//!   editor that never returns from its own drawing (Dragonfly Early
//!   Reflections on macOS) freezes this UI for good; in the sandbox the
//!   host's watchdog kills that process, the device goes silent and
//!   [`PluginRuntime::idle`] tells the user. The sandbox is the safe choice
//!   for plugins whose editors misbehave.
//! * [`RealCatalog`]: stock devices with their real `ParamInfo`s, factory
//!   presets, and the plugin scan (background thread, cached in the app
//!   data folder).

use super::main_thread::MainThread;
use crate::bridge::{DeviceCategory, DeviceInfo, ExternalPlugin, PluginCatalog, PluginEdit, PluginEditKind, PluginFolder, PluginProblem, PluginReport, PresetInfo, ScanStatus};
use aitrack_dsp::{Category, ParamInfo, PluginInfo, ProcessContext, Processor};
use aitrack_engine::ProcessorFactory;
use aitrack_host::sandbox::{SandboxEditor, SandboxedPlugin};
use aitrack_host::scan::Scanner;
use aitrack_host::{HostedPlugin, HostedPluginInfo, ParamEdit, ParamEditKind, PluginController};
use aitrack_model::{Device, DeviceId, DeviceKind};
use crossbeam_channel::{Receiver, bounded, unbounded};
use parking_lot::{Mutex, RwLock};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

/// Loading a plugin can take a while (copy protection, big sample sets).
const LOAD_TIMEOUT: Duration = Duration::from_secs(30);
const CALL_TIMEOUT: Duration = Duration::from_secs(10);

// ───────────────────────────── Stock ─────────────────────────────

/// Stock devices from `aitrack-stock`.
pub struct StockFactory;

impl ProcessorFactory for StockFactory {
    fn create(&self, device: &Device, _sample_rate: f64, _max_frames: usize) -> Option<Box<dyn Processor>> {
        match &device.kind {
            DeviceKind::Stock { plugin_id } => aitrack_stock::create(plugin_id),
            _ => None,
        }
    }
}

fn category_for(id: &str, category: Category) -> DeviceCategory {
    match id {
        "fx.eq" => DeviceCategory::Eq,
        "fx.compressor" | "fx.limiter" | "fx.gate" | "fx.transient" => DeviceCategory::Dynamics,
        "fx.saturator" | "fx.tape" | "fx.amp" | "fx.console" => DeviceCategory::Distortion,
        "fx.clipper" => DeviceCategory::Distortion,
        "fx.multiband" | "fx.midside" => DeviceCategory::Utility,
        "fx.reverb" | "fx.delay" => DeviceCategory::Space,
        "fx.filter" => DeviceCategory::Filter,
        "fx.chorus" | "fx.pitchshift" | "fx.freqshift" | "fx.vocoder" => DeviceCategory::Modulation,
        "instrument.drum" => DeviceCategory::Drums,
        _ if category == Category::Instrument => DeviceCategory::Instrument,
        _ => DeviceCategory::Utility,
    }
}

/// Every stock device with its real parameter list, plus the three racks.
pub fn stock_devices() -> Vec<DeviceInfo> {
    let mut out: Vec<DeviceInfo> = aitrack_stock::registry()
        .iter()
        .map(|p| {
            let proc = (p.create)();
            DeviceInfo {
                id: p.id,
                name: p.name,
                category: category_for(p.id, p.category),
                tags: p.tags,
                params: proc.info().params.clone(),
                is_instrument: p.category == Category::Instrument,
            }
        })
        .collect();
    let rack = |id, name, tags, is_instrument| DeviceInfo { id, name, category: DeviceCategory::Rack, tags, params: Vec::new(), is_instrument };
    out.push(rack("rack.instrument", "Instrument Rack", &["rack", "macros"], true));
    out.push(rack("rack.drum", "Drum Rack", &["rack", "drums", "pads"], true));
    out.push(rack("rack.audio", "Audio Effect Rack", &["rack", "macros"], false));
    out
}

// ───────────────────────────── Hosted ─────────────────────────────

/// Plugin instances the UI can reach, keyed by device.
#[derive(Default)]
struct Instances {
    controllers: HashMap<DeviceId, (u64, PluginController)>,
    sandboxed: HashMap<DeviceId, (u64, SandboxEditor)>,
    /// Edits made in each plugin's own GUI (lock-free queues filled on the
    /// audio thread), drained on the UI tick.
    edits: HashMap<DeviceId, (u64, rtrb::Consumer<ParamEdit>)>,
    /// Each live instance's parameter list, for the automation chooser.
    params: HashMap<DeviceId, (u64, Vec<ParamInfo>)>,
}

/// State shared by the hosted plugin factories, the catalogue and the UI.
pub struct PluginRuntime {
    pub main: MainThread,
    /// Scan results, to look up full plugin descriptions by uid.
    known: RwLock<Vec<HostedPluginInfo>>,
    instances: Mutex<Instances>,
    pub sandbox: AtomicBool,
    messages: Mutex<Vec<String>>,
    /// Per-device runtime warning (hung by watchdog, crashed, failed to start).
    warnings: Mutex<HashMap<DeviceId, (u64, String)>>,
    /// Sticky banner alert when a sandboxed plugin hung or crashed.
    watchdog_alert: Mutex<Option<String>>,
    /// GUI edits drained from the queues, waiting for the UI to turn them
    /// into commands.
    edits: Mutex<Vec<PluginEdit>>,
    next_token: AtomicU64,
}

/// More than this many pending GUI edits (nobody draining) are dropped,
/// oldest first.
const MAX_PENDING_EDITS: usize = 8192;

fn plugin_edit(device: DeviceId, e: ParamEdit) -> PluginEdit {
    let kind = match e.kind {
        ParamEditKind::Begin => PluginEditKind::Begin,
        ParamEditKind::Value => PluginEditKind::Value,
        ParamEditKind::End => PluginEditKind::End,
    };
    PluginEdit { device, param: e.id, value: e.value, kind }
}

impl PluginRuntime {
    pub fn new(main: MainThread, sandbox: bool) -> Arc<Self> {
        // Plugin editor windows reopen at the size and place the user left them
        // (sandboxed hosts get the same file through their environment).
        aitrack_host::editor::placement::set_file(crate::appearance::config_dir().join("plugin_windows.json"));
        // The first time a plugin's editor is opened, its picture is kept here (never during the scan).
        aitrack_host::thumbs::set_dir(crate::views::palette::thumbs::dir());
        Arc::new(Self {
            main,
            known: RwLock::new(Vec::new()),
            instances: Mutex::new(Instances::default()),
            sandbox: AtomicBool::new(sandbox),
            messages: Mutex::new(Vec::new()),
            warnings: Mutex::new(HashMap::new()),
            watchdog_alert: Mutex::new(None),
            edits: Mutex::new(Vec::new()),
            next_token: AtomicU64::new(1),
        })
    }

    pub fn set_known(&self, plugins: Vec<HostedPluginInfo>) {
        *self.known.write() = plugins;
    }

    pub fn message(&self, text: impl Into<String>) {
        self.messages.lock().push(text.into());
    }

    pub fn take_messages(&self) -> Vec<String> {
        std::mem::take(&mut *self.messages.lock())
    }

    pub fn set_device_warning(&self, device: DeviceId, token: u64, msg: impl Into<String>) {
        let text = msg.into();
        self.warnings.lock().insert(device, (token, text));
    }

    pub fn device_warning(&self, device: DeviceId) -> Option<String> {
        self.warnings.lock().get(&device).map(|(_, s)| s.clone())
    }

    pub fn watchdog_warning(&self) -> Option<String> {
        self.watchdog_alert.lock().clone()
    }

    pub fn dismiss_watchdog_warning(&self) {
        *self.watchdog_alert.lock() = None;
    }

    /// The full description of a project's plugin device.
    fn info_for(&self, device: &Device) -> Option<HostedPluginInfo> {
        let DeviceKind::Plugin { format, uid, path, vendor, is_instrument } = &device.kind else { return None };
        if let Some(i) = self.known.read().iter().find(|i| i.format == *format && i.uid == *uid) {
            return Some(i.clone());
        }
        Some(HostedPluginInfo {
            format: *format,
            uid: uid.clone(),
            name: device.name.clone(),
            vendor: vendor.clone(),
            version: String::new(),
            category: String::new(),
            is_instrument: *is_instrument,
            path: path.clone(),
            inputs: 2,
            outputs: 2,
            params: 0,
            probed: false,
        })
    }

    /// Main thread: run queued plugin work and editor housekeeping, and
    /// report sandboxed plugins whose host stopped responding.
    pub fn idle(&self) -> bool {
        let ran = self.main.pump();
        let hung: Vec<(DeviceId, u64, &'static str)> = {
            let mut inst = self.instances.lock();
            inst.sandboxed
                .iter_mut()
                .filter_map(|(&device, (token, e))| e.take_hung().then_some((device, *token, e.name())))
                .collect()
        };
        for &(device, token, name) in &hung {
            let msg = format!("{name} stopped responding and was killed by the sandbox watchdog. It stays silent until you remove and re-add it.");
            self.set_device_warning(device, token, msg.clone());
            *self.watchdog_alert.lock() = Some(msg.clone());
            self.message(msg);
        }
        let controllers: Vec<PluginController> = self.instances.lock().controllers.values().map(|(_, c)| c.clone()).collect();
        for c in controllers {
            c.idle();
        }
        let mut drained = Vec::new();
        for (device, (_, queue)) in self.instances.lock().edits.iter_mut() {
            while let Ok(e) = queue.pop() {
                drained.push(plugin_edit(*device, e));
            }
        }
        let got = !drained.is_empty();
        if got {
            let mut edits = self.edits.lock();
            edits.extend(drained);
            let excess = edits.len().saturating_sub(MAX_PENDING_EDITS);
            edits.drain(..excess);
        }
        ran > 0 || got || !hung.is_empty()
    }

    /// The parameters of a live hosted device (empty when it is not loaded).
    pub fn device_params(&self, device: DeviceId) -> Vec<ParamInfo> {
        self.instances.lock().params.get(&device).map(|(_, p)| p.clone()).unwrap_or_default()
    }

    /// Edits made in plugin editors since the last call, oldest first.
    pub fn take_edits(&self) -> Vec<PluginEdit> {
        std::mem::take(&mut *self.edits.lock())
    }

    /// Main thread: the current state of every live in-process plugin, so
    /// the project can store it before saving. Plugins that fail to save
    /// are left out (their stored state stays as it was).
    pub fn capture_states(&self) -> Vec<(DeviceId, Vec<u8>)> {
        let controllers: Vec<(DeviceId, PluginController)> =
            self.instances.lock().controllers.iter().map(|(d, (_, c))| (*d, c.clone())).collect();
        let mut out = Vec::with_capacity(controllers.len());
        for (device, c) in controllers {
            match c.save_state() {
                Ok(state) if !state.is_empty() => out.push((device, state)),
                Ok(_) => {}
                Err(e) => log::warn!("saving the state of {}: {e}", c.info().name),
            }
        }
        out
    }

    /// Main thread: open a hosted device's editor.
    pub fn open_editor(&self, device: DeviceId) -> Result<(), String> {
        let (controller, sandboxed) = {
            let i = self.instances.lock();
            (i.controllers.get(&device).map(|(_, c)| c.clone()), i.sandboxed.get(&device).map(|(_, e)| e.clone()))
        };
        if let Some(c) = controller {
            if !c.has_editor() {
                return Err(format!("{} has no editor", c.info().name));
            }
            return c.open_editor().map_err(|e| e.to_string());
        }
        if let Some(e) = sandboxed {
            return e.open().map_err(|e| e.to_string());
        }
        Err("The plugin is not loaded (see the status bar for load errors)".into())
    }

    fn token(&self) -> u64 {
        self.next_token.fetch_add(1, Ordering::Relaxed)
    }

    fn forget(&self, device: DeviceId, token: u64) {
        let mut i = self.instances.lock();
        if i.controllers.get(&device).is_some_and(|(t, _)| *t == token) {
            i.controllers.remove(&device);
        }
        if i.sandboxed.get(&device).is_some_and(|(t, _)| *t == token) {
            i.sandboxed.remove(&device);
        }
        if i.edits.get(&device).is_some_and(|(t, _)| *t == token) {
            i.edits.remove(&device);
        }
        if i.params.get(&device).is_some_and(|(t, _)| *t == token) {
            i.params.remove(&device);
        }
        let mut w = self.warnings.lock();
        if w.get(&device).is_some_and(|(t, _)| *t == token) {
            w.remove(&device);
        }
    }
}

/// VST3 / CLAP devices through `aitrack-host`.
pub struct HostedFactory {
    rt: Arc<PluginRuntime>,
    /// Live engine instances register their editor handles; export renders don't.
    live: bool,
}

impl HostedFactory {
    pub fn new(rt: Arc<PluginRuntime>, live: bool) -> Self {
        Self { rt, live }
    }
}

impl ProcessorFactory for HostedFactory {
    fn create(&self, device: &Device, _sample_rate: f64, _max_frames: usize) -> Option<Box<dyn Processor>> {
        let info = self.rt.info_for(device)?;
        let name = info.name.clone();
        let token = self.rt.token();
        if self.rt.sandbox.load(Ordering::Relaxed) {
            return match SandboxedPlugin::spawn(&info) {
                Ok(mut p) => {
                    if self.live {
                        let mut i = self.rt.instances.lock();
                        i.sandboxed.insert(device.id, (token, p.editor_handle()));
                        i.params.insert(device.id, (token, p.info().params.clone()));
                        if let Some(q) = p.take_param_edits() {
                            i.edits.insert(device.id, (token, q));
                        }
                    }
                    Some(Box::new(Tracked { inner: p, rt: self.rt.clone(), key: self.live.then_some((device.id, token)) }))
                }
                Err(e) => {
                    let msg = format!("Could not start {name} in the sandbox: {e}");
                    if self.live {
                        self.rt.set_device_warning(device.id, token, msg.clone());
                        *self.rt.watchdog_alert.lock() = Some(msg.clone());
                    }
                    self.rt.message(msg);
                    None
                }
            };
        }
        let loaded = self.rt.main.run(LOAD_TIMEOUT, move || HostedPlugin::load(&info).map_err(|e| e.to_string()));
        match loaded {
            Some(Ok(mut plugin)) => {
                let controller = plugin.controller();
                if self.live {
                    let mut i = self.rt.instances.lock();
                    i.controllers.insert(device.id, (token, controller.clone()));
                    i.params.insert(device.id, (token, plugin.info().params.clone()));
                    if let Some(q) = plugin.take_param_edits() {
                        i.edits.insert(device.id, (token, q));
                    }
                }
                Some(Box::new(MainThreadPlugin::new(plugin, Some(controller), self.rt.clone(), self.live.then_some((device.id, token)))))
            }
            Some(Err(e)) => {
                let msg = format!("Could not load {name}: {e}");
                if self.live {
                    self.rt.set_device_warning(device.id, token, msg.clone());
                }
                self.rt.message(msg);
                None
            }
            None => {
                let msg = format!("Loading {name} timed out");
                if self.live {
                    self.rt.set_device_warning(device.id, token, msg.clone());
                }
                self.rt.message(msg);
                None
            }
        }
    }
}

/// A sandboxed plugin that unregisters its editor handle when dropped.
struct Tracked {
    inner: SandboxedPlugin,
    rt: Arc<PluginRuntime>,
    key: Option<(DeviceId, u64)>,
}

impl Drop for Tracked {
    fn drop(&mut self) {
        if let Some((d, t)) = self.key {
            self.rt.forget(d, t);
        }
    }
}

impl Processor for Tracked {
    fn info(&self) -> &PluginInfo {
        self.inner.info()
    }
    fn prepare(&mut self, sample_rate: f64, max_frames: usize) {
        self.inner.prepare(sample_rate, max_frames);
    }
    fn reset(&mut self) {
        self.inner.reset();
    }
    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        self.inner.process(ctx);
    }
    fn set_param(&mut self, id: u32, normalized: f32) {
        self.inner.set_param(id, normalized);
    }
    fn get_param(&self, id: u32) -> f32 {
        self.inner.get_param(id)
    }
    fn latency(&self) -> u32 {
        self.inner.latency()
    }
    fn tail(&self) -> u32 {
        self.inner.tail()
    }
    fn save_state(&self) -> Vec<u8> {
        self.inner.save_state()
    }
    fn load_state(&mut self, data: &[u8]) {
        self.inner.load_state(data);
    }
}

/// An in-process hosted plugin whose main-thread calls (`prepare`, state,
/// drop) are sent to the UI thread, so it works from the engine's control
/// thread. Processing stays where the engine calls it (the audio thread).
///
/// If `prepare` takes longer than the timeout the plugin is still on its way
/// through the main thread: until it comes back the signal passes through
/// untouched, parameters set meanwhile are kept and applied on arrival, a
/// newer `prepare` (other rate or block size) is run by the same job before
/// it hands the plugin back, and the audio thread picks the instance up the
/// moment the job finishes. (A plugin that is lost for good is the only case
/// that mutes.)
struct MainThreadPlugin<P: Processor + 'static = HostedPlugin> {
    inner: Option<P>,
    /// Where a plugin whose `prepare` ran late comes back. Kept for the
    /// plugin's whole life: the audio thread must not free a channel.
    returning: Option<Receiver<P>>,
    /// The job that had the plugin was dropped: it will not come back.
    lost: bool,
    /// Talks to the job on the main thread (never touched by the audio thread).
    job: Arc<Mutex<JobState>>,
    /// Parameters set while the instance is away, applied when it is back.
    /// Preallocated for every parameter of the plugin: never grows.
    pending: Vec<(u32, f32)>,
    info: PluginInfo,
    controller: Option<PluginController>,
    rt: Arc<PluginRuntime>,
    key: Option<(DeviceId, u64)>,
    prepare_timeout: Duration,
}

/// The conversation between a late `prepare` job and its owner.
#[derive(Default)]
struct JobState {
    /// A newer `prepare` the job must run before handing the plugin back.
    wanted: Option<(f64, usize)>,
    /// The plugin is in the channel (set together with the send).
    finished: bool,
    /// The owner is gone: release the plugin on the main thread instead.
    abandoned: bool,
}

impl<P: Processor + 'static> MainThreadPlugin<P> {
    fn new(plugin: P, controller: Option<PluginController>, rt: Arc<PluginRuntime>, key: Option<(DeviceId, u64)>) -> Self {
        let info = plugin.info().clone();
        let pending = Vec::with_capacity(info.params.len().max(16));
        Self {
            inner: Some(plugin),
            returning: None,
            lost: false,
            job: Arc::default(),
            pending,
            info,
            controller,
            rt,
            key,
            prepare_timeout: LOAD_TIMEOUT,
        }
    }

    /// Take the instance back if its late `prepare` has finished, and give it
    /// the parameters that arrived meanwhile. No locks, no allocation: a
    /// lock-free `try_recv` and moves.
    #[inline]
    fn recover(&mut self) {
        if self.inner.is_none()
            && !self.lost
            && let Some(rx) = &self.returning
        {
            match rx.try_recv() {
                Ok(mut p) => {
                    for (id, value) in self.pending.drain(..) {
                        p.set_param(id, value);
                    }
                    self.inner = Some(p);
                }
                Err(crossbeam_channel::TryRecvError::Disconnected) => self.lost = true,
                Err(crossbeam_channel::TryRecvError::Empty) => {}
            }
        }
    }
}

impl<P: Processor + 'static> Processor for MainThreadPlugin<P> {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, sample_rate: f64, max_frames: usize) {
        self.recover();
        if self.inner.is_none() && !self.lost && self.returning.is_some() {
            // Still away on the main thread. If its job has not handed it
            // back yet, that job prepares it again for this configuration.
            let mut job = self.job.lock();
            if !job.finished {
                job.wanted = Some((sample_rate, max_frames));
                return;
            }
            drop(job);
            self.recover();
        }
        if let Some(mut p) = self.inner.take() {
            let (tx, rx) = bounded::<P>(1);
            let state: Arc<Mutex<JobState>> = Arc::default();
            self.job = state.clone();
            self.returning = None;
            let run = move || {
                let mut config = (sample_rate, max_frames);
                p.prepare(config.0, config.1);
                loop {
                    let mut s = state.lock();
                    match s.wanted.take() {
                        Some(newer) if newer != config => {
                            drop(s);
                            config = newer;
                            p.prepare(config.0, config.1);
                        }
                        Some(_) => {}
                        None if s.abandoned => {
                            // Nobody wants it any more: release it here, on the main thread.
                            drop(s);
                            drop(p);
                            return;
                        }
                        None => {
                            // Sent while the lock is held, so `finished` means "in the channel".
                            s.finished = true;
                            let _ = tx.send(p);
                            return;
                        }
                    }
                }
            };
            match self.rt.main.run_or_later(self.prepare_timeout, run) {
                Ok(()) => self.inner = rx.try_recv().ok(),
                Err(_) => {
                    self.returning = Some(rx);
                    self.rt.message(format!("{} is slow to activate; its signal passes through until it is ready", self.info.name));
                }
            }
            if self.inner.is_none() && self.returning.is_none() {
                self.lost = true;
            }
        }
    }

    fn reset(&mut self) {
        self.recover();
        if let Some(p) = &mut self.inner {
            p.reset();
        }
    }

    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        self.recover();
        match &mut self.inner {
            Some(p) => p.process(ctx),
            // Still activating: the signal goes by untouched. Only a plugin
            // that can never come back is silent.
            None if self.returning.is_some() && !self.lost => {}
            None => ctx.buffer.clear(),
        }
    }

    fn set_param(&mut self, id: u32, normalized: f32) {
        self.recover();
        match &mut self.inner {
            Some(p) => p.set_param(id, normalized),
            // Away: remember the latest value of each parameter (the list
            // has room for all of them, so it never reallocates).
            None if self.returning.is_some() && !self.lost => {
                if let Some(i) = self.pending.iter().position(|(k, _)| *k == id) {
                    self.pending[i].1 = normalized;
                } else if self.pending.len() < self.pending.capacity() {
                    self.pending.push((id, normalized));
                }
            }
            None => {}
        }
    }

    fn get_param(&self, id: u32) -> f32 {
        match &self.inner {
            Some(p) => p.get_param(id),
            None => self.pending.iter().find(|(k, _)| *k == id).map_or(0.0, |(_, v)| *v),
        }
    }

    fn latency(&self) -> u32 {
        self.inner.as_ref().map_or(0, |p| p.latency())
    }

    fn tail(&self) -> u32 {
        self.inner.as_ref().map_or(0, |p| p.tail())
    }

    fn save_state(&self) -> Vec<u8> {
        let Some(c) = self.controller.clone() else { return Vec::new() };
        self.rt.main.run(CALL_TIMEOUT, move || c.save_state().unwrap_or_default()).unwrap_or_default()
    }

    fn load_state(&mut self, data: &[u8]) {
        // State goes through the controller on the main thread, which does
        // not need the instance to be here: it is not lost while it is away.
        let Some(c) = self.controller.clone() else { return };
        let data = data.to_vec();
        let _ = self.rt.main.run(CALL_TIMEOUT, move || c.load_state(&data).is_ok());
    }
}

impl<P: Processor + 'static> Drop for MainThreadPlugin<P> {
    fn drop(&mut self) {
        if let Some((d, t)) = self.key {
            self.rt.forget(d, t);
        }
        // Dropped elsewhere a hosted plugin leaks on purpose; destroy it on
        // its main thread instead.
        if let Some(p) = self.inner.take() {
            self.rt.main.post(move || drop(p));
        }
        // One that is still away: its job releases it on the main thread, or
        // (already in the channel) it is taken out and sent there.
        if let Some(rx) = &self.returning {
            let mut job = self.job.lock();
            if job.finished {
                if let Ok(p) = rx.try_recv() {
                    self.rt.main.post(move || drop(p));
                }
            } else {
                job.abandoned = true;
            }
        }
    }
}

// ───────────────────────────── Catalogue ─────────────────────────────

enum ScanMsg {
    Progress(f32, String),
    Done(Result<(Vec<HostedPluginInfo>, String, PluginReport), String>),
}

/// What a finished scan found and left out, for Preferences.
fn build_report(scanner: &Scanner, took: Option<Duration>) -> PluginReport {
    let folders = scanner
        .search_roots()
        .into_iter()
        .map(|r| PluginFolder { path: r.path, format: r.format, custom: r.custom, exists: r.exists, files: r.files })
        .collect();
    let problems = scanner
        .problems()
        .into_iter()
        .map(|p| PluginProblem { path: p.path, format: p.format, title: p.kind.title().to_owned(), advice: p.kind.advice().to_owned(), detail: p.message })
        .collect();
    PluginReport { folders, problems, files: scanner.discover().len(), took }
}

/// The one line under "Plugin folders".
fn scan_summary(plugins: usize, files: usize, problems: usize, took: Option<Duration>) -> String {
    let mut s = format!("{plugins} plugin{} in {files} file{}", if plugins == 1 { "" } else { "s" }, if files == 1 { "" } else { "s" });
    if problems > 0 {
        s.push_str(&format!(" · {problems} left out"));
    }
    if let Some(t) = took.filter(|t| t.as_millis() > 0) {
        s.push_str(&format!(" · {:.1} s", t.as_secs_f32()));
    }
    s
}

pub struct RealCatalog {
    stock: Vec<DeviceInfo>,
    external: Vec<ExternalPlugin>,
    rt: Arc<PluginRuntime>,
    cache_path: PathBuf,
    folders: Vec<PathBuf>,
    scan: Option<Receiver<ScanMsg>>,
    status: ScanStatus,
    report: PluginReport,
    /// Files to check again (they were blocked) at the next scan.
    retry: Vec<PathBuf>,
}

fn external(infos: &[HostedPluginInfo]) -> Vec<ExternalPlugin> {
    let mut v: Vec<ExternalPlugin> = infos
        .iter()
        .map(|i| ExternalPlugin {
            uid: i.uid.clone(),
            name: i.name.clone(),
            vendor: i.vendor.clone(),
            format: i.format,
            category: i.category.clone(),
            path: i.path.clone(),
            is_instrument: i.is_instrument,
        })
        .collect();
    v.sort_by_key(|p| p.name.to_lowercase());
    v
}

impl RealCatalog {
    /// Lists the cached plugins right away and starts a background rescan.
    pub fn new(rt: Arc<PluginRuntime>, cache_path: PathBuf) -> Self {
        let cached = Scanner::new(&cache_path).plugins();
        let folders = crate::plugin_prefs::PluginPrefs::load().folders;
        rt.set_known(cached.clone());
        let mut this = Self {
            stock: stock_devices(),
            external: external(&cached),
            rt,
            cache_path,
            folders,
            scan: None,
            status: ScanStatus { running: false, fraction: 0., message: format!("{} plugins (cached)", cached.len()) },
            report: PluginReport::default(),
            retry: Vec::new(),
        };
        this.rescan();
        this
    }
}

impl PluginCatalog for RealCatalog {
    fn stock_devices(&self) -> &[DeviceInfo] {
        &self.stock
    }

    fn external_plugins(&self) -> &[ExternalPlugin] {
        &self.external
    }

    fn device_params(&self, device: DeviceId) -> Vec<ParamInfo> {
        self.rt.device_params(device)
    }

    fn presets(&self, plugin_id: &str) -> Vec<PresetInfo> {
        aitrack_stock::presets::list(plugin_id)
            .into_iter()
            .map(|r| PresetInfo { path: r.path, category: r.category, name: r.name })
            .collect()
    }

    fn load_preset(&self, path: &str) -> Result<(String, BTreeMap<u32, f32>), String> {
        let p = aitrack_stock::presets::load(path).map_err(|e| e.to_string())?;
        let params = p.params.iter().filter_map(|(k, v)| Some((k.parse::<u32>().ok()?, v.clamp(0., 1.)))).collect();
        Ok((p.name, params))
    }

    fn rescan(&mut self) {
        if self.status.running {
            return;
        }
        let (tx, rx) = unbounded();
        let path = self.cache_path.clone();
        let custom = self.folders.clone();
        let retry = std::mem::take(&mut self.retry);
        let spawned = std::thread::Builder::new().name("aitrack-plugin-scan".into()).spawn(move || {
            let mut scanner = Scanner::new(&path);
            for dir in &custom {
                scanner.add_search_path(aitrack_model::PluginFormat::Vst3, dir);
                scanner.add_search_path(aitrack_model::PluginFormat::Clap, dir);
            }
            for p in &retry {
                scanner.unblock(p);
            }
            let started = std::time::Instant::now();
            let files = scanner.discover();
            // One pass over every file with the checkers running side by side (a slow
            // plugin holds only its own worker), then the pass that prunes what is gone
            // and saves: everything is fresh by then.
            let tx_progress = tx.clone();
            let total = files.len();
            scanner.scan_files_with(&files, &move |done, _| {
                let _ = tx_progress.send(ScanMsg::Progress(done as f32 / total.max(1) as f32, format!("Scanning plugins… {done}/{total}")));
            });
            let result = scanner
                .scan()
                .map(|_| {
                    let plugins = scanner.plugins();
                    let report = build_report(&scanner, Some(started.elapsed()));
                    let summary = scan_summary(plugins.len(), report.files, report.problems.len(), report.took);
                    (plugins, summary, report)
                })
                .map_err(|e| e.to_string());
            let _ = tx.send(ScanMsg::Done(result));
        });
        if spawned.is_ok() {
            self.scan = Some(rx);
            self.status = ScanStatus { running: true, fraction: 0., message: "Looking for plugins…".into() };
        }
    }

    fn add_plugin_folder(&mut self, path: PathBuf) -> bool {
        let mut prefs = crate::plugin_prefs::PluginPrefs::load();
        if !prefs.add_folder(path.clone()) {
            return false;
        }
        self.folders = prefs.folders;
        true
    }

    fn remove_plugin_folder(&mut self, path: &Path) -> bool {
        let mut prefs = crate::plugin_prefs::PluginPrefs::load();
        if !prefs.remove_folder(path) {
            return false;
        }
        self.folders = prefs.folders;
        true
    }

    fn scan_status(&self) -> ScanStatus {
        self.status.clone()
    }

    fn plugin_report(&self) -> PluginReport {
        self.report.clone()
    }

    fn retry_plugin(&mut self, path: &Path) {
        self.retry.push(path.to_path_buf());
        self.rescan();
    }

    fn poll(&mut self) -> bool {
        let Some(rx) = &self.scan else { return false };
        let mut changed = false;
        while let Ok(msg) = rx.try_recv() {
            changed = true;
            match msg {
                ScanMsg::Progress(f, m) => self.status = ScanStatus { running: true, fraction: f, message: m },
                ScanMsg::Done(Ok((plugins, summary, report))) => {
                    self.report = report;
                    self.external = external(&plugins);
                    self.rt.set_known(plugins);
                    self.status = ScanStatus { running: false, fraction: 1., message: summary };
                    self.scan = None;
                    break;
                }
                ScanMsg::Done(Err(e)) => {
                    self.status = ScanStatus { running: false, fraction: 0., message: format!("Plugin scan failed: {e}") };
                    self.scan = None;
                    break;
                }
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_scan_summary_says_what_was_left_out_and_how_long_it_took() {
        assert_eq!(scan_summary(1, 1, 0, None), "1 plugin in 1 file");
        assert_eq!(scan_summary(38, 40, 2, Some(Duration::from_millis(1090))), "38 plugins in 40 files · 2 left out · 1.1 s");
    }

    #[test]
    fn the_report_lists_missing_folders_and_files_that_could_not_be_checked() {
        let dir = std::env::temp_dir().join(format!("aitrack-plugin-report-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("mine")).unwrap();
        std::fs::write(dir.join("mine").join("Odd.clap"), b"x").unwrap();
        let mut scanner = Scanner::new(dir.join("cache.json")).with_scanner_exe(dir.join("no-scanner")).without_default_paths();
        scanner.add_search_path(aitrack_model::PluginFormat::Clap, dir.join("mine"));
        scanner.add_search_path(aitrack_model::PluginFormat::Clap, dir.join("gone"));
        scanner.scan().unwrap();
        let r = build_report(&scanner, None);
        assert_eq!(r.files, 1);
        assert_eq!(r.problems.len(), 1);
        assert_eq!(r.problems[0].title, "Not checked");
        assert!(!r.problems[0].advice.is_empty());
        let gone = r.folders.iter().find(|f| f.path.ends_with("gone")).unwrap();
        assert!(gone.custom && !gone.exists);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stock_catalog_has_real_params_and_presets() {
        let devices = stock_devices();
        let eq = devices.iter().find(|d| d.id == "fx.eq").expect("eq");
        assert!(!eq.params.is_empty());
        assert!(devices.iter().any(|d| d.id == "instrument.wavetable" && d.is_instrument));
        let presets = aitrack_stock::presets::list("instrument.wavetable");
        assert!(!presets.is_empty());
        let d = Device::stock(DeviceId(1), "fx.reverb", "Reverb");
        assert!(StockFactory.create(&d, 48_000., 256).is_some());
    }

    /// A processor that needs the main thread to activate and halves the
    /// signal once it has. It checks the block it is given against the
    /// configuration it was prepared with, keeps its parameters, and notes
    /// which thread destroyed it.
    struct Halver {
        info: PluginInfo,
        config: Option<(f64, usize)>,
        params: std::collections::HashMap<u32, f32>,
        dropped_on: Arc<Mutex<Option<std::thread::ThreadId>>>,
    }

    impl Halver {
        fn new() -> Self {
            Self::watched(Arc::default())
        }

        fn watched(dropped_on: Arc<Mutex<Option<std::thread::ThreadId>>>) -> Self {
            let info = PluginInfo {
                id: "test.halver",
                name: "Halver",
                vendor: "test",
                category: Category::Effect,
                tags: &[],
                params: (0..8).map(|id| ParamInfo::new(id, "p", 0.0, 1.0, 0.0)).collect(),
                inputs: 2,
                outputs: 2,
                has_sidechain: false,
            };
            Self { info, config: None, params: Default::default(), dropped_on }
        }
    }

    impl Drop for Halver {
        fn drop(&mut self) {
            *self.dropped_on.lock() = Some(std::thread::current().id());
        }
    }

    impl Processor for Halver {
        fn info(&self) -> &PluginInfo {
            &self.info
        }
        fn prepare(&mut self, sample_rate: f64, max_frames: usize) {
            self.config = Some((sample_rate, max_frames));
        }
        fn reset(&mut self) {}
        fn process(&mut self, ctx: &mut ProcessContext<'_>) {
            let (rate, max) = self.config.expect("processed before activation");
            assert_eq!(ctx.sample_rate, rate, "processing at a rate it was not prepared for");
            assert!(ctx.buffer.frames() <= max, "a block of {} frames, prepared for {max}", ctx.buffer.frames());
            for ch in 0..ctx.buffer.channels() {
                ctx.buffer.channel_mut(ch).iter_mut().for_each(|s| *s *= 0.5);
            }
        }
        fn set_param(&mut self, id: u32, normalized: f32) {
            self.params.insert(id, normalized);
        }
        fn get_param(&self, id: u32) -> f32 {
            self.params.get(&id).copied().unwrap_or(0.0)
        }
    }

    fn run_block_at(p: &mut dyn Processor, rate: f64, frames: usize) -> f32 {
        let mut buf = aitrack_dsp::AudioBuffer::new(2, frames);
        buf.channel_mut(0).fill(1.0);
        buf.channel_mut(1).fill(1.0);
        let transport = aitrack_dsp::Transport::default();
        p.process(&mut ProcessContext { sample_rate: rate, buffer: &mut buf, sidechain: None, events: &[], transport: &transport });
        buf.channel(0)[10]
    }

    fn run_block(p: &mut dyn Processor) -> f32 {
        run_block_at(p, 48_000.0, 64)
    }

    /// A plugin whose first `prepare` timed out, still waiting for the main thread.
    fn away(rt: &Arc<PluginRuntime>, plugin: Halver) -> MainThreadPlugin<Halver> {
        let mut plugin = MainThreadPlugin::new(plugin, None, rt.clone(), None);
        plugin.prepare_timeout = Duration::from_millis(50);
        // The control thread prepares; this thread is "main" and does not pump.
        let control = std::thread::spawn(move || {
            plugin.prepare(48_000.0, 256);
            plugin
        });
        control.join().unwrap()
    }

    /// A hosted plugin whose `prepare` outlasts the timeout used to leave its
    /// track silent for good. The signal now passes through, and the plugin
    /// takes over once the main thread has run the job.
    #[test]
    fn a_slow_prepare_passes_the_signal_through_and_the_plugin_comes_back() {
        // This thread is "main"; the control thread prepares, the audio thread processes.
        let rt = PluginRuntime::new(MainThread::new(), false);
        let started = std::time::Instant::now();
        let mut plugin = away(&rt, Halver::new());
        assert!(started.elapsed() >= Duration::from_millis(50) && started.elapsed() < Duration::from_secs(5));
        assert!(rt.take_messages().iter().any(|m| m.contains("Halver")), "the user is told");
        // Meanwhile the track keeps sounding, untouched.
        assert_eq!(run_block(&mut plugin), 1.0, "the signal must pass through, not be silenced");
        assert_eq!(run_block(&mut plugin), 1.0);
        // The main thread gets to the job: the plugin is activated and takes over.
        assert_eq!(rt.main.pump(), 1);
        assert_eq!(run_block(&mut plugin), 0.5, "the recovered plugin processes");
        assert_eq!(run_block(&mut plugin), 0.5);
        // A prepare that is in time is as before.
        let mut quick = MainThreadPlugin::new(Halver::new(), None, rt.clone(), None);
        quick.prepare(48_000.0, 256);
        assert_eq!(run_block(&mut quick), 0.5);
    }

    /// The engine applies the parameters right after `prepare` and counts
    /// them as applied: those set while the plugin is away must still arrive.
    #[test]
    fn parameters_set_while_the_plugin_is_away_reach_it_on_return() {
        let rt = PluginRuntime::new(MainThread::new(), false);
        let mut plugin = away(&rt, Halver::new());
        let capacity = plugin.pending.capacity();
        plugin.set_param(3, 0.2);
        plugin.set_param(4, 0.4);
        plugin.set_param(3, 0.7);
        for id in 0..100 {
            plugin.set_param(id, 0.1); // more than it has room for: dropped, never grown
        }
        assert_eq!(plugin.pending.capacity(), capacity, "the list must not reallocate");
        plugin.set_param(3, 0.7);
        plugin.set_param(4, 0.4);
        assert_eq!(rt.main.pump(), 1);
        assert_eq!(run_block(&mut plugin), 0.5);
        assert_eq!(plugin.get_param(3), 0.7, "the last value wins");
        assert_eq!(plugin.get_param(4), 0.4);
        assert!(plugin.pending.is_empty());
    }

    /// A second `prepare` (new rate or block size) while the first is still
    /// pending must not be forgotten: the plugin comes back prepared for it.
    #[test]
    fn a_newer_prepare_while_away_is_honoured() {
        let rt = PluginRuntime::new(MainThread::new(), false);
        let mut plugin = away(&rt, Halver::new());
        plugin.prepare(96_000.0, 1024);
        assert_eq!(rt.main.pump(), 1);
        // Prepared for 96 kHz and 1024 frames, not the 48 kHz / 256 of the first call.
        assert_eq!(run_block_at(&mut plugin, 96_000.0, 1024), 0.5);

        // The job may also have finished by the time the new request comes:
        // the instance is fetched and prepared again.
        let mut plugin = away(&rt, Halver::new());
        assert_eq!(rt.main.pump(), 1);
        plugin.prepare_timeout = Duration::from_secs(10);
        let control = std::thread::spawn(move || {
            plugin.prepare(44_100.0, 512);
            plugin
        });
        while rt.main.pump() == 0 && !control.is_finished() {
            std::thread::yield_now();
        }
        let mut plugin = control.join().unwrap();
        assert_eq!(run_block_at(&mut plugin, 44_100.0, 512), 0.5);
    }

    /// A wrapper destroyed while its plugin is away releases the instance on
    /// the main thread, whether the job is still queued or has finished.
    #[test]
    fn an_abandoned_plugin_is_released_on_the_main_thread() {
        let rt = PluginRuntime::new(MainThread::new(), false);
        let main_id = std::thread::current().id();
        for job_ran_first in [false, true] {
            let dropped_on = Arc::new(Mutex::new(None));
            let plugin = away(&rt, Halver::watched(dropped_on.clone()));
            if job_ran_first {
                assert_eq!(rt.main.pump(), 1);
            }
            // The engine's graveyard thread drops the wrapper.
            std::thread::spawn(move || drop(plugin)).join().unwrap();
            rt.main.pump();
            assert_eq!(*dropped_on.lock(), Some(main_id), "job_ran_first = {job_ran_first}: released off the main thread");
        }
    }

    #[test]
    fn targeted_audition_plays_stock_synth_and_loaded_sampler() {
        use aitrack_dsp::AudioBuffer;
        use aitrack_engine::{Engine, EngineConfig, MidiMessage};
        use aitrack_model::{Color, Project, Track, TrackKind};
        use std::time::{SystemTime, UNIX_EPOCH};

        let path = std::env::temp_dir().join(format!(
            "aitrack-audition-{}-{}.wav",
            std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
        ));
        let file = std::fs::OpenOptions::new().write(true).create_new(true).open(&path).unwrap();
        let spec = hound::WavSpec { channels: 1, sample_rate: 48_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut wav = hound::WavWriter::new(file, spec).unwrap();
        for i in 0..8192 {
            let sample = (i as f32 * std::f32::consts::TAU * 440. / 48_000.).sin() * 16_000.;
            wav.write_sample(sample as i16).unwrap();
        }
        wav.finalize().unwrap();
        let mut sampler = aitrack_stock::instrument::sampler::Sampler::new();
        sampler.load_wav(path.to_str().unwrap()).unwrap();
        let state = sampler.save_state();

        for (plugin, state) in [("instrument.wavetable", None), ("instrument.sampler", Some(state))] {
            let mut project = Project::new("audition");
            let track_id = project.alloc_track_id();
            let device_id = project.alloc_device_id();
            let mut track = Track::new(track_id, TrackKind::Instrument, plugin, Color(0));
            let mut device = Device::stock(device_id, plugin, plugin);
            device.state = state;
            track.devices.push(device);
            project.tracks.push(track);
            let (handle, processor) = Engine::new(EngineConfig::headless(48_000.), Arc::new(StockFactory)).unwrap();
            let mut processor = processor.unwrap();
            handle.update_project(&project);
            handle.flush();
            let mut buf = AudioBuffer::new(2, 256);
            processor.process_planar(&mut buf);
            assert!(handle.send_midi_to(track_id, MidiMessage::note_on(0, 60, 100)));
            processor.process_planar(&mut buf);
            assert!(buf.channel(0).iter().any(|s| s.abs() > 0.001), "{plugin} should sound");
            assert!(handle.send_midi_to(track_id, MidiMessage::note_off(0, 60)));
            for _ in 0..400 {
                processor.process_planar(&mut buf);
            }
            assert!(buf.channel(0).iter().all(|s| s.abs() < 0.0001), "{plugin} should release");
        }
        std::fs::remove_file(path).unwrap();
    }
}
