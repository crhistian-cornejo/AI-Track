//! Processor factories and the plugin catalogue over `aitrack-stock` and
//! `aitrack-host`.
//!
//! * [`StockFactory`]: `DeviceKind::Stock` → `aitrack_stock::create`.
//! * [`HostedFactory`]: `DeviceKind::Plugin` → a VST3/CLAP plugin, in this
//!   process (loaded on the main thread, see [`MainThread`]) or in the
//!   `aitrack-plugin-host` sandbox when the setting is on.
//! * [`RealCatalog`]: stock devices with their real `ParamInfo`s, factory
//!   presets, and the plugin scan (background thread, cached in the app
//!   data folder).

use super::main_thread::MainThread;
use crate::bridge::{DeviceCategory, DeviceInfo, ExternalPlugin, PluginCatalog, PresetInfo, ScanStatus};
use aitrack_dsp::{Category, PluginInfo, ProcessContext, Processor};
use aitrack_engine::ProcessorFactory;
use aitrack_host::sandbox::{SandboxEditor, SandboxedPlugin};
use aitrack_host::scan::Scanner;
use aitrack_host::{HostedPlugin, HostedPluginInfo, PluginController};
use aitrack_model::{Device, DeviceId, DeviceKind};
use crossbeam_channel::{Receiver, unbounded};
use parking_lot::{Mutex, RwLock};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
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
        "fx.saturator" => DeviceCategory::Distortion,
        "fx.clipper" => DeviceCategory::Distortion,
        "fx.multiband" | "fx.midside" => DeviceCategory::Utility,
        "fx.reverb" | "fx.delay" => DeviceCategory::Space,
        "fx.filter" => DeviceCategory::Filter,
        "fx.chorus" => DeviceCategory::Modulation,
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
}

/// State shared by the hosted plugin factories, the catalogue and the UI.
pub struct PluginRuntime {
    pub main: MainThread,
    /// Scan results, to look up full plugin descriptions by uid.
    known: RwLock<Vec<HostedPluginInfo>>,
    instances: Mutex<Instances>,
    pub sandbox: AtomicBool,
    messages: Mutex<Vec<String>>,
    next_token: AtomicU64,
}

impl PluginRuntime {
    pub fn new(main: MainThread, sandbox: bool) -> Arc<Self> {
        Arc::new(Self {
            main,
            known: RwLock::new(Vec::new()),
            instances: Mutex::new(Instances::default()),
            sandbox: AtomicBool::new(sandbox),
            messages: Mutex::new(Vec::new()),
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

    /// Main thread: run queued plugin work and editor housekeeping.
    pub fn idle(&self) -> bool {
        let ran = self.main.pump();
        let controllers: Vec<PluginController> = self.instances.lock().controllers.values().map(|(_, c)| c.clone()).collect();
        for c in controllers {
            c.idle();
        }
        ran > 0
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
                Ok(p) => {
                    if self.live {
                        self.rt.instances.lock().sandboxed.insert(device.id, (token, p.editor_handle()));
                    }
                    Some(Box::new(Tracked { inner: p, rt: self.rt.clone(), key: self.live.then_some((device.id, token)) }))
                }
                Err(e) => {
                    self.rt.message(format!("Could not start {name} in the sandbox: {e}"));
                    None
                }
            };
        }
        let loaded = self.rt.main.run(LOAD_TIMEOUT, move || HostedPlugin::load(&info).map_err(|e| e.to_string()));
        match loaded {
            Some(Ok(plugin)) => {
                let controller = plugin.controller();
                if self.live {
                    self.rt.instances.lock().controllers.insert(device.id, (token, controller.clone()));
                }
                let info = plugin.info().clone();
                Some(Box::new(MainThreadPlugin {
                    inner: Some(plugin),
                    info,
                    controller,
                    rt: self.rt.clone(),
                    key: self.live.then_some((device.id, token)),
                }))
            }
            Some(Err(e)) => {
                self.rt.message(format!("Could not load {name}: {e}"));
                None
            }
            None => {
                self.rt.message(format!("Loading {name} timed out"));
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
struct MainThreadPlugin {
    inner: Option<HostedPlugin>,
    info: PluginInfo,
    controller: PluginController,
    rt: Arc<PluginRuntime>,
    key: Option<(DeviceId, u64)>,
}

impl Processor for MainThreadPlugin {
    fn info(&self) -> &PluginInfo {
        &self.info
    }

    fn prepare(&mut self, sample_rate: f64, max_frames: usize) {
        if let Some(mut p) = self.inner.take() {
            self.inner = self.rt.main.run(LOAD_TIMEOUT, move || {
                p.prepare(sample_rate, max_frames);
                p
            });
            if self.inner.is_none() {
                self.rt.message(format!("{} did not activate in time", self.info.name));
            }
        }
    }

    fn reset(&mut self) {
        if let Some(p) = &mut self.inner {
            p.reset();
        }
    }

    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        match &mut self.inner {
            Some(p) => p.process(ctx),
            None => ctx.buffer.clear(),
        }
    }

    fn set_param(&mut self, id: u32, normalized: f32) {
        if let Some(p) = &mut self.inner {
            p.set_param(id, normalized);
        }
    }

    fn get_param(&self, id: u32) -> f32 {
        self.inner.as_ref().map_or(0.0, |p| p.get_param(id))
    }

    fn latency(&self) -> u32 {
        self.inner.as_ref().map_or(0, |p| p.latency())
    }

    fn tail(&self) -> u32 {
        self.inner.as_ref().map_or(0, |p| p.tail())
    }

    fn save_state(&self) -> Vec<u8> {
        let c = self.controller.clone();
        self.rt.main.run(CALL_TIMEOUT, move || c.save_state().unwrap_or_default()).unwrap_or_default()
    }

    fn load_state(&mut self, data: &[u8]) {
        let c = self.controller.clone();
        let data = data.to_vec();
        let _ = self.rt.main.run(CALL_TIMEOUT, move || c.load_state(&data).is_ok());
    }
}

impl Drop for MainThreadPlugin {
    fn drop(&mut self) {
        if let Some((d, t)) = self.key {
            self.rt.forget(d, t);
        }
        // Dropped elsewhere a hosted plugin leaks on purpose; destroy it on
        // its main thread instead.
        if let Some(p) = self.inner.take() {
            self.rt.main.post(move || drop(p));
        }
    }
}

// ───────────────────────────── Catalogue ─────────────────────────────

enum ScanMsg {
    Progress(f32, String),
    Done(Result<(Vec<HostedPluginInfo>, String), String>),
}

pub struct RealCatalog {
    stock: Vec<DeviceInfo>,
    external: Vec<ExternalPlugin>,
    rt: Arc<PluginRuntime>,
    cache_path: PathBuf,
    scan: Option<Receiver<ScanMsg>>,
    status: ScanStatus,
}

fn external(infos: &[HostedPluginInfo]) -> Vec<ExternalPlugin> {
    let mut v: Vec<ExternalPlugin> = infos
        .iter()
        .map(|i| ExternalPlugin {
            uid: i.uid.clone(),
            name: i.name.clone(),
            vendor: i.vendor.clone(),
            format: i.format,
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
        rt.set_known(cached.clone());
        let mut this = Self {
            stock: stock_devices(),
            external: external(&cached),
            rt,
            cache_path,
            scan: None,
            status: ScanStatus { running: false, fraction: 0., message: format!("{} plugins (cached)", cached.len()) },
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
        let spawned = std::thread::Builder::new().name("aitrack-plugin-scan".into()).spawn(move || {
            let mut scanner = Scanner::new(&path);
            let files = scanner.discover();
            let n = files.len().max(1);
            // Small batches so the dialog can show progress (each batch is
            // scanned out of process, a few in parallel).
            let mut done = 0;
            for chunk in files.chunks(4) {
                let _ = tx.send(ScanMsg::Progress(done as f32 / n as f32, format!("Scanning plugins… {done}/{}", files.len())));
                scanner.scan_files(chunk);
                done += chunk.len();
            }
            // Prunes vanished files and saves the cache (everything is fresh now).
            let result = scanner
                .scan()
                .map(|report| {
                    let plugins = scanner.plugins();
                    let failed = report.failed.len() + report.skipped_blocked;
                    let summary = if failed > 0 {
                        format!("{} plugins · {failed} failed or blocked", plugins.len())
                    } else {
                        format!("{} plugins", plugins.len())
                    };
                    (plugins, summary)
                })
                .map_err(|e| e.to_string());
            let _ = tx.send(ScanMsg::Done(result));
        });
        if spawned.is_ok() {
            self.scan = Some(rx);
            self.status = ScanStatus { running: true, fraction: 0., message: "Looking for plugins…".into() };
        }
    }

    fn scan_status(&self) -> ScanStatus {
        self.status.clone()
    }

    fn poll(&mut self) -> bool {
        let Some(rx) = &self.scan else { return false };
        let mut changed = false;
        while let Ok(msg) = rx.try_recv() {
            changed = true;
            match msg {
                ScanMsg::Progress(f, m) => self.status = ScanStatus { running: true, fraction: f, message: m },
                ScanMsg::Done(Ok((plugins, summary))) => {
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
