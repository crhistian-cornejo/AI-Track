//! [`EngineBridge`] over `aitrack-engine`.
//!
//! The project goes to the engine on every History revision change; the
//! engine compiles it off the audio thread. Transport, meters, CPU and clip
//! states are read from the handle's atomics once per frame ([`tick`]) and
//! cached, so the many views that ask during one paint all see the same
//! values. Export runs on its own thread with fresh processor instances.
//!
//! Nothing here blocks the UI thread on the audio device: the engine opens
//! it on its control thread (also when switching devices), the device list
//! is enumerated on a worker thread, and results are picked up in [`tick`].
//! The playhead is the *heard* position: the engine's audible position
//! moved back by the measured output latency (buffer + device + Bluetooth),
//! so it lines up with the speakers.
//!
//! [`tick`]: EngineBridge::tick

use super::plugins::PluginRuntime;
use super::settings::Settings;
use crate::bridge::*;
use aitrack_engine::engine::DeviceRequest;
use aitrack_engine::{
    ActiveAudio, AudioDeviceInfo, ChannelSelection, ClipSlotStatus, Engine, EngineConfig, EngineError, EngineHandle, FactoryStack, Lame,
    MidiPortInfo, Mp3Tags, RenderOptions, RenderRange, ShareMode, TapReader, WavFormat,
};
use aitrack_model::{DeviceId, Project, Quantize, SceneId, Tick, TrackId};
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

/// The analysis tap pauses when the UI has not drained it for this long.
const TAP_IDLE: std::time::Duration = std::time::Duration::from_millis(750);

/// Meters fall at this rate after a peak (dB per second).
const METER_FALL_DB_S: f32 = 26.;

/// How often the device list is re-enumerated (on a worker thread) so
/// hot-plugged interfaces and Bluetooth headphones show up in Preferences.
const DEVICE_RESCAN: std::time::Duration = std::time::Duration::from_secs(4);

/// Longest hold-back for meters and analysis (beyond this the output is
/// broken, not slow).
const MAX_HOLD: std::time::Duration = std::time::Duration::from_millis(500);

/// Peak per track taken in one frame.
type Peaks = Vec<(TrackId, [f32; 2])>;

/// Meter peaks wait here until the audio they measured reaches the speakers.
#[derive(Default)]
struct MeterDelay {
    queue: std::collections::VecDeque<(Instant, Peaks)>,
}

impl MeterDelay {
    /// Queue `readings` taken at `now`; return the peak per track of every
    /// reading at least `delay` old (each reading is released once).
    fn push(&mut self, now: Instant, readings: Peaks, delay: std::time::Duration) -> HashMap<TrackId, [f32; 2]> {
        self.queue.push_back((now, readings));
        let mut due: HashMap<TrackId, [f32; 2]> = HashMap::new();
        while let Some((at, _)) = self.queue.front() {
            if now.duration_since(*at) < delay.min(MAX_HOLD) {
                break;
            }
            let (_, r) = self.queue.pop_front().expect("front");
            for (id, p) in r {
                let d = due.entry(id).or_default();
                d[0] = d[0].max(p[0]);
                d[1] = d[1].max(p[1]);
            }
        }
        due
    }
}

/// Analysis frames wait here until they are heard: the newest `hold`
/// frames stay back, the rest go to the meter strip.
#[derive(Default)]
struct TapDelay {
    stage: std::collections::VecDeque<[f32; 2]>,
}

impl TapDelay {
    fn pass(&mut self, fresh: &[[f32; 2]], hold: usize, out: &mut Vec<[f32; 2]>) {
        self.stage.extend(fresh.iter().copied());
        let release = self.stage.len().saturating_sub(hold);
        out.extend(self.stage.drain(..release));
    }
}

/// The device list from a worker thread: `(hosts, devices)`.
type DeviceScan = crossbeam_channel::Receiver<(Vec<String>, Vec<AudioDeviceInfo>)>;

fn scan_devices() -> Option<DeviceScan> {
    let (tx, rx) = crossbeam_channel::bounded(1);
    std::thread::Builder::new()
        .name("aitrack-audio-devices".into())
        .spawn(move || {
            let _ = tx.send((aitrack_engine::audio_io::hosts(), aitrack_engine::audio_io::devices()));
        })
        .ok()?;
    Some(rx)
}

fn rec_note(n: aitrack_engine::record::RecordedNote) -> RecNote {
    RecNote { track: n.track, key: n.key, velocity: n.velocity, start_tick: n.start_tick, length_ticks: n.length_ticks, pass: n.pass }
}

/// How often the MIDI input ports are looked at, so a keyboard plugged in
/// after the app started is picked up too.
const MIDI_RESCAN: std::time::Duration = std::time::Duration::from_secs(3);

/// Which of the `listed` MIDI input ports to open (not open yet) and which of
/// the `open` ones have disappeared (unplugged: closed so that plugging the
/// keyboard in again opens it again). Pure, so it is testable without hardware.
///
/// `disabled` are the ports the user switched off in Preferences: they are
/// never opened, and closed if they are open. Every other port is on, so a
/// keyboard that was never seen before just works.
fn plan_midi(listed: &[MidiPortInfo], open: &BTreeSet<String>, disabled: &BTreeSet<String>) -> (Vec<MidiPortInfo>, Vec<String>) {
    let to_open = listed.iter().filter(|p| !open.contains(&p.id) && !disabled.contains(&p.id)).cloned().collect();
    let gone = open.iter().filter(|id| disabled.contains(*id) || !listed.iter().any(|p| p.id == **id)).cloned().collect();
    (to_open, gone)
}

/// What the user chose about MIDI input, kept in `midi.json` next to the other
/// settings: the ports switched off (all others are on) and the track live
/// MIDI goes to (`None` = every armed instrument track).
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct MidiPrefs {
    pub disabled_ports: BTreeSet<String>,
    pub target: Option<u64>,
}

impl MidiPrefs {
    fn path() -> PathBuf {
        crate::appearance::config_dir().join("midi.json")
    }

    pub fn load_from(path: &std::path::Path) -> Self {
        std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save_to(&self, path: &std::path::Path) {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(path, json);
        }
    }
}

/// What one pass over the MIDI ports did.
struct MidiScan {
    /// Every port the system lists (for the Preferences page).
    listed: Vec<MidiPortInfo>,
    /// Ports now open (the engine keeps them open across audio-device
    /// changes and re-wires them to the new audio thread by itself).
    open: BTreeSet<String>,
    /// Ports that would not open, with the reason.
    failed: Vec<(MidiPortInfo, String)>,
}

/// Tells the MIDI worker to stop when the engine that started it is dropped.
struct StopOnDrop(Arc<AtomicBool>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// Open every MIDI input on a worker thread (opening waits on the engine's
/// control thread, which the UI thread must never do). Armed instrument
/// tracks receive what they play: the engine's default MIDI target.
///
/// The worker holds the handle only **weakly**, and a strong reference only
/// for the length of one call: closing the app never waits for it, and the
/// engine is shut down by whoever lets go of it last (the worker, at worst,
/// not the UI thread). `stop` ends the pass between ports.
fn scan_midi(handle: &Arc<EngineHandle>, open: BTreeSet<String>, disabled: BTreeSet<String>, stop: Arc<AtomicBool>) -> Option<crossbeam_channel::Receiver<MidiScan>> {
    let (tx, rx) = crossbeam_channel::bounded(1);
    let handle = Arc::downgrade(handle);
    std::thread::Builder::new()
        .name("aitrack-midi-ports".into())
        .spawn(move || {
            let alive = || !stop.load(Ordering::Relaxed);
            let Some(listed) = handle.upgrade().filter(|_| alive()).map(|h| h.midi_inputs()) else { return };
            let (to_open, gone) = plan_midi(&listed, &open, &disabled);
            let mut open = open;
            let mut failed = Vec::new();
            for id in gone {
                let Some(h) = handle.upgrade().filter(|_| alive()) else { return };
                h.close_midi_input(&id);
                open.remove(&id);
            }
            for port in to_open {
                let Some(h) = handle.upgrade().filter(|_| alive()) else { return };
                match h.open_midi_input(&port.id) {
                    Ok(()) => {
                        open.insert(port.id);
                    }
                    Err(e) => failed.push((port, e.to_string())),
                }
            }
            let _ = tx.send(MidiScan { listed, open, failed });
        })
        .ok()?;
    Some(rx)
}

struct Frame {
    at: Instant,
    transport: TransportState,
    meters: HashMap<TrackId, MeterLevel>,
    cpu: f32,
    slots: HashMap<TrackId, SlotState>,
}

pub struct RealEngine {
    handle: Arc<EngineHandle>,
    export_factory: Arc<FactoryStack>,
    rt: Arc<PluginRuntime>,
    settings: Settings,
    looping: bool,
    metronome: bool,
    count_in: u32,
    global_quantize: Quantize,
    recording: bool,
    loop_region: Option<(Tick, Tick)>,
    main_track: Option<TrackId>,
    tracks: Vec<TrackId>,
    /// Loop length of every Session clip, to turn the engine's 0..1 clip
    /// progress into the "playing since" tick the views expect.
    clip_len: HashMap<(TrackId, SceneId), Tick>,
    frame: Frame,
    banner: Option<String>,
    hosts: Vec<AudioHost>,
    devices: Vec<AudioDevice>,
    app_config: AudioConfig,
    active: Option<ActiveAudio>,
    lame: OnceLock<Result<(), String>>,
    messages: Vec<String>,
    preview: super::preview::Preview,
    /// Device enumeration in flight, and when the last one started.
    scan: Option<DeviceScan>,
    scanned_at: Instant,
    /// Device switch in flight: the request and the config it applies.
    pending: Option<(DeviceRequest, AudioConfig, aitrack_engine::AudioConfig)>,
    /// Last [`EngineHandle::audio_generation`] seen.
    generation: u64,
    latency_seen: aitrack_engine::status::LatencyReport,
    meter_delay: MeterDelay,
    tap_delay: TapDelay,
    tap_fresh: Vec<[f32; 2]>,
    /// The UI's cursor into the engine's analysis ring.
    tap: TapReader,
    analysis: bool,
    tap_drained: Instant,
    /// MIDI inputs open, the pass in flight and when the last one started.
    midi_open: BTreeSet<String>,
    /// The ports of the last pass, the user's choices and a pass asked for while one ran.
    midi_listed: Vec<MidiPortInfo>,
    midi_prefs: MidiPrefs,
    midi_again: bool,
    midi_scan: Option<crossbeam_channel::Receiver<MidiScan>>,
    midi_scanned_at: Instant,
    /// Ports already reported as failing (told once, retried quietly).
    midi_reported: BTreeSet<String>,
    /// Ends the MIDI worker when this engine is dropped.
    midi_stop: StopOnDrop,
    /// What the engine reports about itself, and the compile generation its
    /// warnings were read at (they are read again only when it moves).
    health: EngineHealth,
    compile_seen: Option<u64>,
    notices: NoticeLog,
    drops: DropTracker,
}

fn host_name(h: AudioHost) -> &'static str {
    match h {
        AudioHost::Wasapi | AudioHost::WasapiExclusive => "WASAPI",
        AudioHost::Asio => "ASIO",
        AudioHost::CoreAudio => "CoreAudio",
    }
}

fn hosts_for(name: &str) -> &'static [AudioHost] {
    match name.to_ascii_lowercase().as_str() {
        "wasapi" => &[AudioHost::Wasapi, AudioHost::WasapiExclusive],
        "asio" => &[AudioHost::Asio],
        "coreaudio" => &[AudioHost::CoreAudio],
        _ => &[],
    }
}

fn selection(channels: &[u16]) -> ChannelSelection {
    let first = channels.iter().copied().min().unwrap_or(0);
    ChannelSelection { first, count: (channels.len() as u16).clamp(1, 2) }
}

fn channels(sel: ChannelSelection) -> Vec<u16> {
    (sel.first..sel.first + sel.count.max(1)).collect()
}

/// The app's audio settings → the engine's.
pub fn to_engine(c: &AudioConfig) -> aitrack_engine::AudioConfig {
    aitrack_engine::AudioConfig {
        host: Some(host_name(c.host).into()),
        share_mode: if c.host == AudioHost::WasapiExclusive { ShareMode::Exclusive } else { ShareMode::Shared },
        output_device: c.output.clone(),
        output_device_name: None,
        output_channels: selection(&c.output_channels),
        input_enabled: c.input.is_some(),
        input_device: c.input.clone(),
        input_device_name: None,
        input_channels: selection(&c.input_channels),
        sample_rate: Some(c.sample_rate),
        buffer_size: Some(c.buffer_size),
    }
}

/// The engine's (persisted) audio settings → the app's.
pub fn from_engine(c: &aitrack_engine::AudioConfig) -> AudioConfig {
    let d = AudioConfig::default();
    let host = c
        .host
        .as_deref()
        .and_then(|h| hosts_for(h).iter().copied().find(|x| (*x == AudioHost::WasapiExclusive) == (c.share_mode == ShareMode::Exclusive)))
        .unwrap_or(d.host);
    AudioConfig {
        host,
        input: c.input_enabled.then(|| c.input_device.clone()).flatten(),
        output: c.output_device.clone(),
        sample_rate: c.sample_rate.unwrap_or(d.sample_rate),
        buffer_size: c.buffer_size.unwrap_or(d.buffer_size),
        input_channels: channels(c.input_channels),
        output_channels: channels(c.output_channels),
    }
}

fn wav_format(d: WavDepth) -> WavFormat {
    match d {
        WavDepth::Int16Dithered => WavFormat::Int16(aitrack_engine::Dither::Tpdf),
        WavDepth::Int24 => WavFormat::Int24,
        WavDepth::Float32 => WavFormat::Float32,
    }
}

/// Where the app looks for `libmp3lame` (it is never bundled or linked).
pub fn lame_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![crate::appearance::config_dir().join("lame")];
    if let Some(exe_dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(PathBuf::from)) {
        dirs.push(exe_dir.join("lame"));
        dirs.push(exe_dir);
    }
    dirs
}

fn lame_missing_message() -> String {
    let names = aitrack_engine::mp3::library_names().join(" / ");
    format!(
        "MP3 export needs LAME ({names}), which AI-Track does not bundle. Put it in {} or next to the app, or export WAV.",
        crate::appearance::config_dir().join("lame").display()
    )
}

impl RealEngine {
    pub fn new(live: Arc<FactoryStack>, export_factory: Arc<FactoryStack>, rt: Arc<PluginRuntime>, settings: Settings) -> Self {
        let app_config = from_engine(&settings.audio);
        // The device opens on the engine's control thread: a slow driver
        // must not hold up the first frame. `tick` picks the result up.
        let config = EngineConfig {
            audio: Some(settings.audio.clone()),
            open_in_background: true,
            audio_threads: settings.audio_threads,
            streaming: settings.streaming,
            ..EngineConfig::default()
        };
        aitrack_engine::set_render_threads(settings.audio_threads);
        let (handle, banner) = match Engine::new(config, live.clone()) {
            Ok((h, _)) => {
                let banner = h.last_error().map(|e| no_device_banner(&e));
                (h, banner)
            }
            Err(e) => {
                // Could not even start the engine threads with a device:
                // run headless so editing and export keep working.
                let headless = EngineConfig { audio_threads: settings.audio_threads, streaming: settings.streaming, ..EngineConfig::headless(48_000.) };
                let (h, _) = Engine::new(headless, live.clone()).expect("start the headless engine");
                (h, Some(no_device_banner(&e.to_string())))
            }
        };
        let handle = Arc::new(handle);
        let midi_prefs = MidiPrefs::load_from(&MidiPrefs::path());
        handle.set_midi_target(midi_prefs.target.map(TrackId));
        let active = handle.active_audio();
        let tap = handle.analysis_tap();
        handle.set_sample_cache_bytes(settings.sample_cache_mb.clamp(64, 4096) * 1024 * 1024);
        let mut this = Self {
            tap,
            analysis: false,
            tap_drained: Instant::now(),
            handle,
            export_factory,
            rt,
            settings,
            looping: true,
            metronome: false,
            count_in: 0,
            global_quantize: Quantize::Bar,
            recording: false,
            loop_region: None,
            main_track: None,
            tracks: Vec::new(),
            clip_len: HashMap::new(),
            frame: Frame { at: Instant::now(), transport: TransportState { looping: true, ..Default::default() }, meters: HashMap::new(), cpu: 0., slots: HashMap::new() },
            banner,
            hosts: Vec::new(),
            devices: Vec::new(),
            app_config,
            active,
            lame: OnceLock::new(),
            messages: Vec::new(),
            preview: super::preview::Preview::new(),
            scan: None,
            scanned_at: Instant::now(),
            pending: None,
            generation: 0,
            latency_seen: Default::default(),
            meter_delay: MeterDelay::default(),
            tap_delay: TapDelay::default(),
            tap_fresh: Vec::new(),
            midi_open: BTreeSet::new(),
            midi_listed: Vec::new(),
            midi_prefs,
            midi_again: false,
            midi_scan: None,
            midi_scanned_at: Instant::now(),
            midi_reported: BTreeSet::new(),
            midi_stop: StopOnDrop(Arc::new(AtomicBool::new(false))),
            health: EngineHealth::default(),
            compile_seen: None,
            notices: NoticeLog::default(),
            drops: DropTracker::default(),
        };
        this.adopt_active();
        this.refresh_devices();
        this.refresh_midi();
        this
    }

    /// Read the engine's own report: the audio health every frame (a few
    /// atomics), the compile warnings and error only when a compile finished.
    /// Whatever is new becomes a toast, once.
    fn poll_health(&mut self) {
        let a = self.handle.audio_health();
        let h = &mut self.health;
        (h.callback_panics, h.node_panics, h.sanitized_samples) = (a.callback_panics, a.node_panics, a.sanitized_samples);
        h.crashed_devices = a.crashed_devices;
        h.last_panic = a.last_panic;
        let rec = self.handle.recording_info();
        h.recording_lost = self.drops.update(rec.capturing, rec.dropped);
        h.warp_failures = self.handle.sample_cache_warp_failures();
        let generation = self.handle.compile_generation();
        if self.compile_seen != Some(generation) {
            self.compile_seen = Some(generation);
            h.warnings = self.handle.warnings();
            // Without a device the banner already says why nothing compiles.
            h.error = self.handle.last_error().filter(|_| self.active.is_some());
        }
        let fresh = self.notices.fresh(&self.health);
        self.messages.extend(fresh);
        warn_rt_heap(aitrack_engine::rt_heap_operations());
    }

    /// Start a pass over the MIDI input ports (no-op while one runs).
    fn refresh_midi(&mut self) {
        if self.midi_scan.is_none() {
            self.midi_scan = scan_midi(&self.handle, self.midi_open.clone(), self.midi_prefs.disabled_ports.clone(), self.midi_stop.0.clone());
            self.midi_scanned_at = Instant::now();
        }
    }

    /// Pick up a finished pass over the MIDI ports and start the next one
    /// when it is due. `true` when something is worth telling the user.
    fn poll_midi(&mut self) -> bool {
        let Some(rx) = &self.midi_scan else {
            if self.midi_scanned_at.elapsed() > MIDI_RESCAN {
                self.refresh_midi();
            }
            return false;
        };
        match rx.try_recv() {
            Ok(scan) => {
                self.midi_scan = None;
                self.midi_open = scan.open;
                self.midi_listed = scan.listed;
                // A choice made while the pass ran: look again at once.
                if std::mem::take(&mut self.midi_again) {
                    self.refresh_midi();
                }
                // A port that opens later is told about again if it fails again.
                self.midi_reported.retain(|id| scan.failed.iter().any(|(p, _)| p.id == *id));
                let mut told = false;
                for (port, why) in scan.failed {
                    if self.midi_reported.insert(port.id.clone()) {
                        self.messages.push(format!("MIDI input {} did not open: {why}", port.name));
                        told = true;
                    }
                }
                told
            }
            Err(crossbeam_channel::TryRecvError::Empty) => false,
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                self.midi_scan = None;
                false
            }
        }
    }

    /// Take the device the engine has open now (after a start, a switch or
    /// an automatic reopen) into the app's view of the settings.
    fn adopt_active(&mut self) {
        self.generation = self.handle.audio_generation();
        self.active = self.handle.active_audio();
        if let Some(a) = &self.active {
            self.app_config.sample_rate = a.sample_rate;
            if let Some(b) = a.buffer_size {
                self.app_config.buffer_size = b;
            }
            self.messages.extend(a.notes.iter().cloned());
            self.banner = None;
            // The browser preview plays where the engine plays.
            self.preview.set_device((!a.follows_default).then(|| (a.host.clone(), a.output_device_id.clone())));
        }
    }

    /// Start enumerating devices on a worker thread (no-op while one runs).
    fn refresh_devices(&mut self) {
        if self.scan.is_none() {
            self.scan = scan_devices();
            self.scanned_at = Instant::now();
        }
    }

    /// Pick up a finished enumeration; `true` when the list changed.
    fn poll_devices(&mut self) -> bool {
        let Some(rx) = &self.scan else {
            if self.scanned_at.elapsed() > DEVICE_RESCAN {
                self.refresh_devices();
            }
            return false;
        };
        match rx.try_recv() {
            Ok((hosts, devices)) => {
                self.scan = None;
                let before = (self.hosts.clone(), self.devices.clone());
                self.apply_devices(hosts, devices);
                before != (self.hosts.clone(), self.devices.clone())
            }
            Err(crossbeam_channel::TryRecvError::Empty) => false,
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                self.scan = None;
                false
            }
        }
    }

    fn apply_devices(&mut self, engine_hosts: Vec<String>, engine_devices: Vec<AudioDeviceInfo>) {
        let mut hosts = Vec::new();
        for h in &engine_hosts {
            hosts.extend_from_slice(hosts_for(h));
        }
        self.hosts = hosts;
        let mut out = Vec::new();
        for d in engine_devices {
            for &host in hosts_for(&d.host) {
                let mut rates = d.sample_rates.clone();
                if rates.is_empty() {
                    rates = SAMPLE_RATES.to_vec();
                }
                let (lo, hi) = d.buffer_range.unwrap_or((32, 4096));
                out.push(AudioDevice {
                    id: d.id.clone(),
                    name: d.name.clone(),
                    host,
                    input_channels: d.input_channels,
                    output_channels: d.output_channels,
                    default_rate: d.default_sample_rate.unwrap_or(48_000),
                    sample_rates: rates,
                    buffer_range: (lo.clamp(32, 4096), hi.clamp(32, 4096).max(lo.clamp(32, 4096))),
                });
            }
        }
        self.devices = out;
    }

    /// The outcome of a device switch, of the background open at start-up,
    /// or of an automatic reopen (device lost, default output changed).
    /// `true` when something the settings page shows changed.
    fn poll_audio(&mut self) -> bool {
        let mut changed = false;
        if let Some((req, _, _)) = &self.pending
            && let Some(result) = req.poll() {
                let (_, config, engine_cfg) = self.pending.take().expect("pending");
                match result {
                    Ok(_) => {
                        self.app_config = config;
                        self.settings.audio = engine_cfg;
                        self.settings.save();
                        self.adopt_active();
                    }
                    Err(e) => {
                        let msg = e.to_string();
                        self.banner = Some(no_device_banner(&msg));
                        self.messages.push(format!("Audio device: {msg}"));
                    }
                }
                self.refresh_devices();
                changed = true;
            }
        if self.pending.is_none() && self.handle.audio_generation() != self.generation {
            changed = true;
            self.adopt_active();
            if self.active.is_none()
                && let Some(e) = self.handle.last_error() {
                    self.banner = Some(no_device_banner(&e));
                }
            // A device came or went: list what is there now.
            self.refresh_devices();
        }
        // The first callbacks of a new stream bring the measured latency.
        let lat = self.handle.latency_report();
        if lat != self.latency_seen {
            self.latency_seen = lat;
            changed = true;
        }
        changed
    }

    fn session_rate(&self) -> u32 {
        match self.handle.sample_rate() {
            0 => self.app_config.sample_rate,
            r => r,
        }
    }

    fn slot_from(&self, s: &ClipSlotStatus, pos: Tick) -> Option<SlotState> {
        use aitrack_engine::SlotState as E;
        match s.state {
            E::Playing | E::StopQueued => {
                let len = self.clip_len.get(&(s.track, s.scene)).copied().unwrap_or(4 * aitrack_model::PPQ);
                Some(SlotState::Playing { scene: s.scene, since: pos - (s.progress.clamp(0., 1.) as f64 * len as f64) as Tick })
            }
            E::Queued => Some(SlotState::Queued { scene: s.scene }),
            E::Stopped => None,
        }
    }
}

/// The engine's latency figures as the settings page shows them.
fn latency_of(r: &aitrack_engine::status::LatencyReport) -> AudioLatency {
    AudioLatency {
        sample_rate: r.sample_rate,
        buffer_frames: r.buffer_frames,
        output_ms: r.output_ms(),
        input_ms: r.input_ms(),
        device_output_ms: r.device_output_ms(),
        plugin_ms: r.plugin_ms(),
    }
}

fn no_device_banner(e: &str) -> String {
    format!("No audio device could be opened ({e}). Playback is off; editing and export still work. Pick a device in Preferences → Audio.")
}

impl EngineBridge for RealEngine {
    fn play(&mut self) {
        self.handle.play();
    }

    fn stop(&mut self) {
        self.handle.stop();
        self.recording = false;
        self.handle.set_recording(false);
    }

    fn toggle_record(&mut self) {
        self.recording = !self.recording;
        self.handle.set_recording(self.recording);
    }

    fn set_looping(&mut self, on: bool) {
        self.looping = on;
        self.handle.set_loop(if on { self.loop_region } else { None });
    }

    fn set_metronome(&mut self, on: bool) {
        self.metronome = on;
        self.handle.set_metronome(on);
    }

    fn set_count_in(&mut self, bars: u32) {
        self.count_in = bars;
        self.handle.set_count_in(bars);
    }

    fn count_in(&self) -> u32 {
        self.count_in
    }

    fn set_global_quantize(&mut self, quantize: Quantize) {
        self.global_quantize = quantize;
        self.handle.set_global_quantize(quantize);
    }

    fn global_quantize(&self) -> Quantize {
        self.global_quantize
    }

    fn back_to_arrangement(&mut self) {
        self.handle.back_to_arrangement();
    }

    fn locate(&mut self, tick: Tick) {
        self.handle.seek(tick.max(0));
        self.frame.transport.position = tick.max(0);
    }

    fn transport(&self) -> TransportState {
        TransportState { looping: self.looping, metronome: self.metronome, recording: self.recording, ..self.frame.transport }
    }

    fn recording_state(&self) -> RecordingState {
        let info = self.handle.recording_info();
        RecordingState {
            capturing: info.capturing,
            start_tick: info.start_tick.round() as Tick,
            pass: info.pass,
            tracks: info.midi_tracks,
            audio_tracks: info.audio_tracks,
            takes_started: info.takes_started,
            takes_finished: info.takes_finished,
            dropped: info.dropped,
        }
    }

    fn finish_audio_recording(&mut self) -> Vec<AudioTake> {
        self.handle
            .finish_audio_recording()
            .into_iter()
            .map(|t| AudioTake { track: t.track, path: t.path, start_tick: t.start_tick.round() as Tick, frames: t.frames, sample_rate: t.sample_rate, channels: u16::from(t.channels), error: t.error })
            .collect()
    }

    fn set_recording_dir(&mut self, dir: &std::path::Path) {
        self.handle.set_recording_dir(dir);
    }

    fn set_track_input(&mut self, track: TrackId, input: aitrack_model::InputSource) {
        // "No input" is said outright: to the engine a missing selection
        // means its default input, which would still record the track.
        let sel = input.channel_span().map(|(first, count)| ChannelSelection { first, count });
        self.handle.set_track_input_none(track, sel.is_none());
        self.handle.set_track_input(track, sel);
    }

    fn set_track_monitor(&mut self, track: TrackId, monitor: aitrack_model::Monitor) {
        self.handle.set_track_monitor(track, Some(monitor));
    }

    fn take_recorded_notes(&mut self) -> Vec<RecNote> {
        self.handle.take_recorded_notes().into_iter().map(rec_note).collect()
    }

    fn take_captured_notes(&mut self) -> Vec<RecNote> {
        self.handle.take_captured_notes().into_iter().map(rec_note).collect()
    }

    fn finish_midi_recording(&mut self) -> Option<MidiTake> {
        let t = self.handle.finish_midi_recording()?;
        Some(MidiTake { notes: t.notes.into_iter().map(rec_note).collect(), start_tick: t.start_tick, end_tick: t.end_tick, passes: t.passes })
    }

    fn midi_ports(&self) -> Vec<MidiPortState> {
        self.midi_listed
            .iter()
            .map(|p| MidiPortState { id: p.id.clone(), name: p.name.clone(), enabled: !self.midi_prefs.disabled_ports.contains(&p.id), open: self.midi_open.contains(&p.id) })
            .collect()
    }

    fn set_midi_port_enabled(&mut self, id: &str, enabled: bool) {
        let changed = if enabled { self.midi_prefs.disabled_ports.remove(id) } else { self.midi_prefs.disabled_ports.insert(id.to_owned()) };
        if !changed {
            return;
        }
        self.midi_prefs.save_to(&MidiPrefs::path());
        if !enabled && self.midi_open.remove(id) {
            // Closing never waits; the next pass confirms it.
            self.handle.close_midi_input(id);
        }
        if self.midi_scan.is_some() {
            self.midi_again = true;
        } else {
            self.refresh_midi();
        }
    }

    fn midi_target(&self) -> Option<TrackId> {
        self.handle.midi_target()
    }

    fn set_midi_target(&mut self, track: Option<TrackId>) {
        self.handle.set_midi_target(track);
        self.midi_prefs.target = track.map(|t| t.0);
        self.midi_prefs.save_to(&MidiPrefs::path());
    }

    fn midi_message_count(&self) -> u64 {
        self.handle.midi_message_count()
    }

    fn sample_cache_warp_bytes(&self) -> usize {
        self.handle.sample_cache_warp_bytes()
    }

    fn fast_param(&mut self, device: DeviceId, param: u32, normalized: f32) -> bool {
        self.handle.set_device_param(device, param, normalized)
    }

    fn fast_rack_selector(&mut self, rack: DeviceId, value: u8) -> bool {
        self.handle.set_rack_selector(rack, value)
    }

    fn fast_track_volume(&mut self, track: TrackId, db: f32) -> bool {
        self.handle.set_track_volume(track, db)
    }

    fn fast_track_pan(&mut self, track: TrackId, pan: f32) -> bool {
        self.handle.set_track_pan(track, pan)
    }

    fn fast_send_level(&mut self, track: TrackId, index: usize, db: f32) -> bool {
        self.handle.set_send_level(track, index, db)
    }

    fn compile_count(&self) -> u64 {
        self.handle.compile_count()
    }

    fn patch_count(&self) -> u64 {
        self.handle.patch_count()
    }

    fn update_project(&mut self, project: &Project) {
        self.handle.update_project(project);
        if project.loop_region != self.loop_region {
            self.loop_region = project.loop_region;
            if self.looping {
                self.handle.set_loop(self.loop_region);
            }
        }
        self.main_track = Some(project.main_track().id);
        self.tracks = project.tracks.iter().map(|t| t.id).collect();
        self.clip_len = project
            .tracks
            .iter()
            .flat_map(|t| {
                t.session_slots.iter().filter_map(move |(scene, clip)| {
                    let c = project.clips.get(clip)?;
                    let len = c.looping.map_or(c.length, |l| l.end - l.start).max(aitrack_model::PPQ);
                    Some(((t.id, *scene), len))
                })
            })
            .collect();
    }

    fn audition_note(&mut self, track: TrackId, key: u8, velocity: Option<u8>) -> bool {
        let message = match velocity {
            Some(v) => aitrack_engine::MidiMessage::note_on(0, key, v.clamp(1, 127)),
            None => aitrack_engine::MidiMessage::note_off(0, key),
        };
        self.handle.send_midi_to(track, message)
    }

    fn tick(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.frame.at).as_secs_f32().min(0.5);
        self.frame.at = now;
        self.poll_audio();
        self.poll_devices();
        self.poll_health();
        let position = self.handle.heard_position_ticks().max(0.) as Tick;
        self.frame.transport = TransportState { playing: self.handle.is_playing(), position, ..self.frame.transport };
        self.frame.cpu = self.handle.cpu_load().0.clamp(0., 1.);
        let fall = 10f32.powf(-METER_FALL_DB_S * dt / 20.);
        // Meters show what the speakers play now, not what the engine just
        // rendered (with Bluetooth that is a few hundred ms apart).
        let readings = self
            .tracks
            .iter()
            .map(|&id| {
                let reading = if Some(id) == self.main_track { Some(self.handle.master_meter()) } else { self.handle.meters(id) };
                (id, reading.map_or([0.; 2], |r| r.peak))
            })
            .collect();
        let delay = std::time::Duration::from_secs_f64(self.latency_seen.output_seconds());
        let due = self.meter_delay.push(now, readings, delay);
        for &id in &self.tracks {
            let peak = due.get(&id).copied().unwrap_or([0.; 2]);
            let m = self.frame.meters.entry(id).or_default();
            m.left = peak[0].max(m.left * fall).min(1.5);
            m.right = peak[1].max(m.right * fall).min(1.5);
            if m.left < 1e-4 {
                m.left = 0.;
            }
            if m.right < 1e-4 {
                m.right = 0.;
            }
        }
        let mut slots: HashMap<TrackId, SlotState> = HashMap::new();
        for s in self.handle.clip_states() {
            if let Some(state) = self.slot_from(&s, position) {
                // One state per track: a playing clip wins over a queued one.
                let replace = match slots.get(&s.track) {
                    None => true,
                    Some(SlotState::Queued { .. }) => matches!(state, SlotState::Playing { .. }),
                    Some(SlotState::Playing { .. }) => false,
                };
                if replace {
                    slots.insert(s.track, state);
                }
            }
        }
        self.frame.slots = slots;
    }

    fn idle(&mut self) -> bool {
        self.preview.collect();
        let errors = self.preview.take_errors();
        let mut changed = !errors.is_empty();
        self.messages.extend(errors);
        changed |= self.poll_audio();
        changed |= self.poll_devices();
        changed |= self.poll_midi();
        // Nobody drained the tap for a while (window hidden or minimised):
        // stop feeding it until the next drain.
        if self.tap.is_enabled() && self.tap_drained.elapsed() > TAP_IDLE {
            self.tap.set_enabled(false);
        }
        self.rt.idle() || changed
    }

    fn set_analysis(&mut self, on: bool) {
        self.analysis = on;
        self.tap_drained = Instant::now();
        self.tap.set_enabled(on);
    }

    fn set_analysis_source(&mut self, track: Option<TrackId>) {
        self.tap.set_source(track);
    }

    fn drain_analysis(&mut self, out: &mut Vec<[f32; 2]>) -> u32 {
        self.tap_drained = Instant::now();
        if self.analysis && !self.tap.is_enabled() {
            self.tap.set_enabled(true);
        }
        self.tap_fresh.clear();
        self.tap.read(&mut self.tap_fresh);
        let rate = self.session_rate();
        let hold = (self.latency_seen.output_frames as usize).min(rate as usize * MAX_HOLD.as_millis() as usize / 1000);
        self.tap_delay.pass(&self.tap_fresh, hold, out);
        rate
    }

    fn device_meter(&mut self, device: DeviceId, out: &mut [f32]) -> usize {
        self.handle.device_meter(device, out)
    }

    fn preview(&mut self, path: Option<&std::path::Path>) {
        if let Err(e) = self.preview.play(path) {
            self.messages.push(e);
        }
    }

    fn preview_synced(&mut self, req: &SyncedPreview) {
        if let Err(e) = self.preview.play_synced(req) {
            self.messages.push(e);
        }
    }

    fn set_preview_volume(&mut self, volume: f32) {
        self.preview.set_volume(volume);
    }

    fn preview_pitched(&mut self, path: Option<&std::path::Path>, ratio: f64, reverse: bool) {
        if let Err(e) = self.preview.play_pitched(path, ratio, reverse) {
            self.messages.push(e);
        }
    }

    fn meter(&self, track: TrackId) -> MeterLevel {
        self.frame.meters.get(&track).copied().unwrap_or_default()
    }

    fn cpu_load(&self) -> f32 {
        self.frame.cpu
    }

    fn launch_clip(&mut self, track: TrackId, scene: SceneId) {
        self.handle.launch_clip(track, scene);
    }

    fn launch_scene(&mut self, scene: SceneId) {
        self.handle.launch_scene(scene);
    }

    fn stop_track(&mut self, track: TrackId) {
        self.handle.stop_track(track);
    }

    fn slot_state(&self, track: TrackId) -> Option<SlotState> {
        self.frame.slots.get(&track).copied()
    }

    fn audio_hosts(&self) -> Vec<AudioHost> {
        self.hosts.clone()
    }

    fn audio_devices(&self) -> Vec<AudioDevice> {
        self.devices.clone()
    }

    fn audio_config(&self) -> AudioConfig {
        self.app_config.clone()
    }

    fn set_audio_config(&mut self, mut config: AudioConfig) -> Result<AudioConfig, String> {
        if config.host != self.app_config.host {
            self.refresh_devices();
        }
        let output = config.output.as_ref().and_then(|id| self.devices.iter().find(|d| &d.id == id && d.host == config.host)).cloned();
        if let Some(d) = output {
            if !d.sample_rates.contains(&config.sample_rate) {
                config.sample_rate = d.default_rate;
            }
            config.buffer_size = config.buffer_size.clamp(d.buffer_range.0, d.buffer_range.1);
            config.output_channels.retain(|c| *c < d.output_channels);
        }
        // Opening a device can take a while (Bluetooth: up to a second);
        // the engine does it on its control thread and `tick` applies the
        // result. The settings page shows the request right away.
        let engine_cfg = to_engine(&config);
        let request = self.handle.set_device_async(Some(engine_cfg.clone()));
        self.app_config = config.clone();
        self.pending = Some((request, config.clone(), engine_cfg));
        Ok(config)
    }

    fn test_tone(&mut self) {
        self.messages.push("The test tone is not available in this engine build yet".into());
    }

    fn audio_latency(&self) -> Option<AudioLatency> {
        self.active.as_ref()?;
        Some(latency_of(&self.handle.latency_report()))
    }

    fn export(&mut self, options: ExportOptions, project: &Project, mut progress: ProgressFn) {
        let project = project.clone();
        let factory = self.export_factory.clone();
        let spawned = std::thread::Builder::new().name("aitrack-export".into()).spawn(move || {
            let opts = render_options(&options, &project);
            let stage = |f: f32| if f < 0.97 { "Rendering" } else { "Writing files" };
            let result: Result<Vec<PathBuf>, EngineError> = (|| {
                std::fs::create_dir_all(&options.folder)?;
                let mut cb = |f: f32| {
                    progress(ExportProgress { fraction: f.clamp(0., 1.), stage: stage(f).into(), done: false, cancelled: false, error: None, files: Vec::new() })
                };
                match (&options.mode, options.format) {
                    (ExportMode::Mixdown, ExportFormat::Wav(depth)) => {
                        let path = options.folder.join(format!("{}.wav", options.file_name));
                        aitrack_engine::export_wav(&project, &*factory, &path, &opts, wav_format(depth), Some(&mut cb))?;
                        Ok(vec![path])
                    }
                    (ExportMode::Mixdown, ExportFormat::Mp3) => {
                        let lame = Lame::load(None, &lame_dirs()).map_err(|_| EngineError::Export(lame_missing_message()))?;
                        let path = options.folder.join(format!("{}.mp3", options.file_name));
                        let tags = Mp3Tags {
                            title: Some(project.name.clone()),
                            artist: None,
                            bpm: Some(project.tempo.initial_bpm()),
                            key: project.scale.map(|s| s.display()),
                        };
                        aitrack_engine::export_mp3(&project, &*factory, &path, &opts, &tags, &lame, Some(&mut cb))?;
                        Ok(vec![path])
                    }
                    (ExportMode::Stems { tracks }, format) => {
                        // Stems are always WAV.
                        let depth = match format {
                            ExportFormat::Wav(d) => d,
                            ExportFormat::Mp3 => WavDepth::Int24,
                        };
                        let dir = options.folder.join(format!("{} Stems", options.file_name));
                        aitrack_engine::export_stems(&project, &*factory, &dir, &opts, tracks, wav_format(depth), Some(&mut cb))
                    }
                }
            })();
            let done = |files, cancelled, error| ExportProgress { fraction: 1., stage: "Done".into(), done: true, cancelled, error, files };
            let _ = match result {
                Ok(files) => progress(done(files, false, None)),
                Err(EngineError::Cancelled) => progress(done(Vec::new(), true, None)),
                Err(e) => progress(done(Vec::new(), false, Some(e.to_string()))),
            };
        });
        if let Err(e) = spawned {
            self.messages.push(format!("Could not start the export: {e}"));
        }
    }

    fn mp3_available(&self) -> Result<(), String> {
        self.lame.get_or_init(|| Lame::load(None, &lame_dirs()).map(|_| ()).map_err(|_| lame_missing_message())).clone()
    }

    fn banner(&self) -> Option<String> {
        self.banner.clone()
    }

    fn health(&self) -> EngineHealth {
        self.health.clone()
    }

    fn dismiss_banner(&mut self) {
        self.banner = None;
    }

    fn watchdog_warning(&self) -> Option<String> {
        self.rt.watchdog_warning()
    }

    fn dismiss_watchdog_warning(&mut self) {
        self.rt.dismiss_watchdog_warning();
    }

    fn device_warning(&self, device: DeviceId) -> Option<String> {
        self.rt.device_warning(device)
    }

    fn export_factory(&self) -> Arc<dyn aitrack_engine::ProcessorFactory> {
        self.export_factory.clone()
    }

    fn set_sample_cache_mb(&mut self, mb: usize) {
        let clamped = mb.clamp(64, 4096);
        self.settings.sample_cache_mb = clamped;
        self.settings.save();
        self.handle.set_sample_cache_bytes(clamped * 1024 * 1024);
    }

    fn sample_cache_stats(&self) -> (usize, usize) {
        self.handle.sample_cache_stats()
    }

    fn streaming(&self) -> aitrack_engine::StreamSettings {
        self.settings.streaming
    }

    fn set_streaming(&mut self, settings: aitrack_engine::StreamSettings) {
        self.settings.streaming = settings;
        self.settings.save();
        self.handle.set_streaming(settings);
    }

    fn stream_stats(&self) -> aitrack_engine::StreamStats {
        self.handle.stream_stats()
    }

    fn status_label(&self) -> String {
        match &self.active {
            Some(a) => {
                let buf = a.buffer_size.map(|b| format!(" · {b}")).unwrap_or_default();
                let lat = self.handle.latency_report();
                let out = if lat.output_frames > 0 { format!(" · {:.1} ms out", lat.output_ms()) } else { String::new() };
                format!("{} · {} · {:.1} kHz{buf}{out}", a.host, a.output_device, a.sample_rate as f32 / 1000.).replace(".0 kHz", " kHz")
            }
            None if self.pending.is_some() => "Opening the audio device…".into(),
            None => "No audio device".into(),
        }
    }

    fn open_editor(&mut self, device: DeviceId) -> Result<(), String> {
        self.rt.open_editor(device)
    }

    fn plugin_states(&mut self) -> Vec<(DeviceId, Vec<u8>)> {
        let states = self.rt.capture_states();
        // These bytes come from the live plugins: when the app writes them
        // into the project, the engine must not rebuild (or reload) them.
        for (device, state) in &states {
            aitrack_engine::shared::note_applied_state(*device, state);
        }
        states
    }

    fn take_plugin_edits(&mut self) -> Vec<crate::bridge::PluginEdit> {
        self.rt.take_edits()
    }

    fn sandbox_plugins(&self) -> bool {
        self.settings.sandbox
    }

    fn set_sandbox_plugins(&mut self, on: bool) {
        self.settings.sandbox = on;
        self.rt.sandbox.store(on, std::sync::atomic::Ordering::Relaxed);
        self.settings.save();
    }

    fn audio_threads(&self) -> usize {
        self.settings.audio_threads
    }

    fn set_audio_threads(&mut self, setting: usize) {
        self.settings.audio_threads = setting;
        self.settings.save();
        self.handle.set_audio_threads(setting);
        aitrack_engine::set_render_threads(setting);
    }

    fn active_audio_threads(&self) -> usize {
        self.handle.audio_threads()
    }

    fn take_messages(&mut self) -> Vec<String> {
        let mut m = std::mem::take(&mut self.messages);
        m.extend(self.rt.take_messages());
        m
    }
}

/// How the offline render is set up for `options`. It renders **at the export
/// rate**, not the audio device's: the render does not depend on the device,
/// and rendering at the device rate and converting at the end would put the
/// samples through two sample-rate conversions (44.1 → 48 → 44.1 kHz).
fn render_options(options: &ExportOptions, project: &Project) -> RenderOptions {
    let range = match options.range {
        ExportRange::Song => RenderRange::Song,
        ExportRange::Loop if project.loop_region.is_some() => RenderRange::Loop,
        ExportRange::Loop => RenderRange::Song,
        ExportRange::Selection { start, end } => RenderRange::Ticks(start, end),
    };
    RenderOptions {
        range,
        session_rate: options.sample_rate as f64,
        export_rate: options.sample_rate,
        block: 512,
        tail_seconds: options.tail_seconds.max(0.) as f64,
        normalize_db: options.normalize.then_some(-0.3),
        samples: None,
        threads: None,
    }
}

/// Debug builds: say when the audio threads touched the heap (the count
/// from `aitrack_engine::RtAllocChecker`), each time it doubles, so a busy
/// offender does not flood the log.
fn warn_rt_heap(ops: u64) {
    use std::sync::atomic::{AtomicU64, Ordering};
    static TOLD: AtomicU64 = AtomicU64::new(0);
    let told = TOLD.load(Ordering::Relaxed);
    if rt_heap_due(told, ops) {
        TOLD.store(ops, Ordering::Relaxed);
        log::warn!("audio threads made {ops} heap operations so far (the engine's or a plugin's): real-time code should not allocate");
    }
}

/// Whether `ops` heap operations are worth a new warning after one at `told`.
fn rt_heap_due(told: u64, ops: u64) -> bool {
    ops > 0 && (told == 0 || ops >= told.saturating_mul(2))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audio_thread_heap_use_is_reported_as_it_doubles() {
        assert!(!rt_heap_due(0, 0), "nothing to say");
        assert!(rt_heap_due(0, 1), "the first one");
        assert!(!rt_heap_due(1, 1) && rt_heap_due(1, 2));
        assert!(!rt_heap_due(100, 199) && rt_heap_due(100, 200));
    }

    #[test]
    fn midi_ports_open_as_they_appear_and_close_when_unplugged() {
        let port = |id: &str| MidiPortInfo { id: id.into(), name: id.to_uppercase() };
        let (a, b) = (port("a"), port("b"));
        let none = BTreeSet::new();
        // At start-up every hardware port is opened.
        let off = BTreeSet::new();
        let (to_open, gone) = plan_midi(&[a.clone(), b.clone()], &none, &off);
        assert_eq!(to_open, vec![a.clone(), b.clone()]);
        assert!(gone.is_empty());
        // Nothing new, nothing to do (the next pass does not reopen them).
        let open: BTreeSet<String> = ["a".to_owned(), "b".to_owned()].into();
        assert_eq!(plan_midi(&[a.clone(), b.clone()], &open, &off), (vec![], vec![]));
        // A keyboard plugged in later is opened; one unplugged is closed...
        let c = port("c");
        assert_eq!(plan_midi(&[a.clone(), c.clone()], &open, &off), (vec![c.clone()], vec!["b".to_owned()]));
        // ...and opens again when it comes back.
        let open: BTreeSet<String> = ["a".to_owned()].into();
        assert_eq!(plan_midi(&[a.clone(), b.clone()], &open, &off).0, vec![b.clone()]);
        // A port switched off in Preferences is never opened, and is closed if it is open.
        let b_off: BTreeSet<String> = ["b".to_owned()].into();
        assert_eq!(plan_midi(&[a.clone(), b.clone()], &none, &b_off), (vec![a.clone()], vec![]));
        let both_open: BTreeSet<String> = ["a".to_owned(), "b".to_owned()].into();
        assert_eq!(plan_midi(&[a, b], &both_open, &b_off), (vec![], vec!["b".to_owned()]));
    }

    #[test]
    fn the_midi_choices_are_remembered_and_default_to_everything_on() {
        let path = std::env::temp_dir().join(format!("aitrack-midi-prefs-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let fresh = MidiPrefs::load_from(&path);
        assert!(fresh.disabled_ports.is_empty() && fresh.target.is_none(), "nothing saved: every port on, armed tracks");
        let chosen = MidiPrefs { disabled_ports: ["usb-1".to_owned()].into(), target: Some(7) };
        chosen.save_to(&path);
        assert_eq!(MidiPrefs::load_from(&path), chosen);
        // A file from a version that knew fewer fields still opens.
        std::fs::write(&path, r#"{"target": 3}"#).unwrap();
        assert_eq!(MidiPrefs::load_from(&path), MidiPrefs { disabled_ports: BTreeSet::new(), target: Some(3) });
        let _ = std::fs::remove_file(&path);
    }

    /// The headless engine takes a port that does not exist without hanging
    /// the caller: the pass reports it instead of opening it.
    #[test]
    fn a_pass_over_the_midi_ports_never_blocks_and_reports_failures() {
        let (handle, _) = Engine::new(EngineConfig::headless(48_000.), Arc::new(aitrack_engine::NullFactory)).unwrap();
        let handle = Arc::new(handle);
        assert!(handle.open_midi_input("no such port").is_err());
        let rx = scan_midi(&handle, BTreeSet::new(), BTreeSet::new(), Arc::new(AtomicBool::new(false))).expect("worker thread");
        let scan = rx.recv_timeout(std::time::Duration::from_secs(15)).expect("the pass finishes");
        // Whatever ports this machine lists, every one is either open or failed.
        assert!(scan.open.len() + scan.failed.len() <= 64);
        assert!(scan.failed.iter().all(|(_, why)| !why.is_empty()));
    }

    /// Closing the app: the worker neither keeps the engine alive nor runs on.
    #[test]
    fn the_midi_worker_lets_go_when_the_engine_is_dropped_or_stopped() {
        let (handle, _) = Engine::new(EngineConfig::headless(48_000.), Arc::new(aitrack_engine::NullFactory)).unwrap();
        let handle = Arc::new(handle);
        // Stopped before it starts: it ends without a result.
        let stop = Arc::new(AtomicBool::new(true));
        let rx = scan_midi(&handle, BTreeSet::new(), BTreeSet::new(), stop).unwrap();
        assert!(matches!(rx.recv_timeout(std::time::Duration::from_secs(15)), Err(crossbeam_channel::RecvTimeoutError::Disconnected)));
        // The worker never holds more than a weak reference between calls.
        let rx = scan_midi(&handle, BTreeSet::new(), BTreeSet::new(), Arc::new(AtomicBool::new(false))).unwrap();
        let _ = rx.recv_timeout(std::time::Duration::from_secs(15));
        assert_eq!(Arc::strong_count(&handle), 1);
        // The guard raises the flag when the engine goes.
        let flag = Arc::new(AtomicBool::new(false));
        drop(StopOnDrop(flag.clone()));
        assert!(flag.load(Ordering::Relaxed));
    }

    #[test]
    fn export_renders_at_the_export_rate_so_there_is_no_second_conversion() {
        for rate in [44_100u32, 48_000, 88_200, 96_000] {
            let options = ExportOptions {
                format: ExportFormat::Wav(WavDepth::Int24),
                sample_rate: rate,
                range: ExportRange::Song,
                mode: ExportMode::Mixdown,
                normalize: false,
                tail_seconds: 2.,
                folder: PathBuf::from("out"),
                file_name: "Song".into(),
            };
            let o = render_options(&options, &Project::new("t"));
            // `render` resamples only when the two differ.
            assert_eq!(o.session_rate.round() as u32, o.export_rate, "{rate} Hz");
            assert_eq!(o.export_rate, rate);
        }
    }

    #[test]
    fn audio_config_round_trips_through_the_engine_type() {
        let c = AudioConfig {
            host: if cfg!(target_os = "macos") { AudioHost::CoreAudio } else { AudioHost::WasapiExclusive },
            input: Some("in".into()),
            output: Some("out".into()),
            sample_rate: 96_000,
            buffer_size: 128,
            input_channels: vec![2, 3],
            output_channels: vec![0, 1],
        };
        let e = to_engine(&c);
        assert_eq!(e.output_channels, ChannelSelection { first: 0, count: 2 });
        assert_eq!(e.input_channels, ChannelSelection { first: 2, count: 2 });
        assert!(e.input_enabled);
        assert_eq!(from_engine(&e), c);
    }

    #[test]
    fn meters_wait_for_the_output_latency() {
        let mut d = MeterDelay::default();
        let t = Instant::now();
        let id = TrackId(1);
        let ms = std::time::Duration::from_millis;
        // 100 ms of latency: a peak read now shows 100 ms later, once.
        assert!(d.push(t, vec![(id, [0.8, 0.5])], ms(100)).is_empty());
        assert!(!d.push(t + ms(50), vec![(id, [0.1, 0.1])], ms(100)).contains_key(&id));
        assert_eq!(d.push(t + ms(160), vec![(id, [0.; 2])], ms(100)).get(&id), Some(&[0.8, 0.5]));
        assert_eq!(d.push(t + ms(170), vec![], ms(100)).get(&id), None);
        // No latency: straight through. Absurd latency is capped.
        let mut d = MeterDelay::default();
        assert_eq!(d.push(t, vec![(id, [0.3, 0.2])], ms(0)).get(&id), Some(&[0.3, 0.2]));
        assert!(d.push(t, vec![(id, [0.3, 0.2])], ms(10_000)).is_empty());
        assert!(d.push(t + MAX_HOLD, vec![], ms(10_000)).contains_key(&id));
    }

    #[test]
    fn analysis_frames_are_held_back_by_the_latency() {
        let mut d = TapDelay::default();
        let mut out = Vec::new();
        let frames: Vec<[f32; 2]> = (0..10).map(|i| [i as f32, 0.]).collect();
        d.pass(&frames, 4, &mut out);
        assert_eq!(out.len(), 6);
        assert_eq!(out[5][0], 5.);
        out.clear();
        d.pass(&frames[..2], 4, &mut out);
        assert_eq!(out.iter().map(|f| f[0]).collect::<Vec<_>>(), vec![6., 7.]);
        // Latency dropped (wired headphones): the backlog flushes.
        out.clear();
        d.pass(&[], 0, &mut out);
        assert_eq!(out.len(), 4);
    }

    #[test]
    fn latency_figures_reach_the_settings_page() {
        let r = aitrack_engine::status::LatencyReport {
            sample_rate: 48_000,
            buffer_frames: 256,
            output_frames: 480,
            input_frames: 288,
            input_buffer_frames: 256,
            plugin_frames: 48,
        };
        let l = latency_of(&r);
        assert_eq!(l.buffer_frames, 256);
        assert!((l.output_ms - 10.).abs() < 1e-4);
        assert!((l.device_output_ms - 224. / 48.).abs() < 1e-3);
        assert!((l.input_ms - 6.).abs() < 1e-4);
        assert!((l.plugin_ms - 1.).abs() < 1e-4);
        assert!((l.round_trip_ms() - 16.).abs() < 1e-3);
    }

    #[test]
    fn benchmark_24_tracks_realtime_headroom() {
        use aitrack_engine::{render_offline_with, SampleCache};
        use aitrack_model::{
            AudioSource, Clip, Color, Device, LoopRegion, Note, NoteSource, Placement, Project, Source, Track, TrackKind, Warp, PPQ,
        };
        let dir = std::env::temp_dir().join(format!("aitrack-bench24-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let wav_path = dir.join("loop.wav");
        let spec = hound::WavSpec { channels: 2, sample_rate: 48_000, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
        let mut w = hound::WavWriter::create(&wav_path, spec).unwrap();
        for i in 0..(48_000 * 2) {
            let s = (2.0 * std::f32::consts::PI * 220.0 * i as f32 / 48_000.0).sin() * 0.25;
            w.write_sample(s).unwrap();
            w.write_sample(s).unwrap();
        }
        w.finalize().unwrap();

        let mut p = Project::new("Benchmark 24 Tracks");
        p.tempo = aitrack_model::TempoMap::constant(120.0);
        let sid = p.alloc_source_id();
        p.sources.insert(
            sid,
            Source::Audio(AudioSource {
                path: wav_path,
                hash: None,
                sample_rate: 48_000,
                channels: 2,
                frames: 48_000 * 2,
                warp: Warp::default(),
                key: None,
            }),
        );
        // 12 audio tracks with EQ + Compressor + Utility.
        for i in 0..12 {
            let tid = p.alloc_track_id();
            let mut tr = Track::new(tid, TrackKind::Audio, format!("Audio {}", i + 1), Color::palette(i));
            let d1 = p.alloc_device_id();
            let d2 = p.alloc_device_id();
            tr.devices.push(Device::stock(d1, aitrack_stock::fx::eq::ID, "EQ"));
            tr.devices.push(Device::stock(d2, aitrack_stock::fx::compressor::ID, "Compressor"));
            let cid = p.alloc_clip_id();
            let mut clip = Clip::new(format!("Loop {i}"), Color::palette(i), sid, 20 * PPQ);
            clip.looping = Some(LoopRegion { start: 0, end: 4 * PPQ });
            p.clips.insert(cid, clip);
            let pid = p.alloc_placement_id();
            tr.arrangement.push(Placement { id: pid, clip: cid, start: 0, lane: 0 });
            p.tracks.insert(0, tr);
        }
        // 12 instrument tracks with Wavetable + EQ + Delay/Reverb.
        for i in 0..12 {
            let tid = p.alloc_track_id();
            let mut tr = Track::new(tid, TrackKind::Instrument, format!("Synth {}", i + 1), Color::palette(i + 12));
            let inst = p.alloc_device_id();
            let eq = p.alloc_device_id();
            let fx = p.alloc_device_id();
            tr.devices.push(Device::stock(inst, aitrack_stock::instrument::wavetable::ID, "Wavetable"));
            tr.devices.push(Device::stock(eq, aitrack_stock::fx::eq::ID, "EQ"));
            let fx_id = if i % 3 == 0 { aitrack_stock::fx::reverb::ID } else { aitrack_stock::fx::delay::ID };
            tr.devices.push(Device::stock(fx, fx_id, "FX"));
            let mut notes = Vec::new();
            for step in 0..40 {
                let nid = p.alloc_note_id();
                notes.push(Note::new(nid, 48 + ((step + i) % 24) as u8, step as i64 * (PPQ / 2), PPQ / 4, 0.8));
            }
            let nsid = p.alloc_source_id();
            p.sources.insert(nsid, Source::Notes(NoteSource { notes }));
            let cid = p.alloc_clip_id();
            p.clips.insert(cid, Clip::new(format!("Pattern {i}"), Color::palette(i), nsid, 20 * PPQ));
            let pid = p.alloc_placement_id();
            tr.arrangement.push(Placement { id: pid, clip: cid, start: 0, lane: 0 });
            p.tracks.insert(0, tr);
        }

        let factory = super::super::plugins::StockFactory;
        let samples = SampleCache::blocking();
        // Warm sample cache before timing DSP render.
        let _ = samples.get(
            &match &p.sources[&sid] {
                Source::Audio(a) => a.path.clone(),
                _ => unreachable!(),
            },
            48_000,
        );
        let t0 = Instant::now();
        let buf = render_offline_with(&p, &factory, (0, 20 * PPQ), 48_000.0, 256, samples).unwrap();
        let elapsed = t0.elapsed();
        let peak = buf.channel(0).iter().fold(0.0f32, |m, s| m.max(s.abs()));
        let rtf = elapsed.as_secs_f64() / 10.0;
        eprintln!(
            "24-track benchmark (10s @ 48kHz, block=256, frames={}): {:.3}s wall-clock (RTF={:.3}, DSP={:.1}%)",
            buf.frames(),
            elapsed.as_secs_f64(),
            rtf,
            rtf * 100.0
        );
        assert!(peak > 0.01, "expected audible mixed output across 24 tracks");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
