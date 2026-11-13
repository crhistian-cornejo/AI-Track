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
    Mp3Tags, RenderOptions, RenderRange, ShareMode, TapReader, WavFormat,
};
use aitrack_model::{DeviceId, Project, SceneId, Tick, TrackId};
use std::collections::HashMap;
use std::path::PathBuf;
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

struct Frame {
    at: Instant,
    transport: TransportState,
    meters: HashMap<TrackId, MeterLevel>,
    cpu: f32,
    slots: HashMap<TrackId, SlotState>,
}

pub struct RealEngine {
    handle: EngineHandle,
    export_factory: Arc<FactoryStack>,
    rt: Arc<PluginRuntime>,
    settings: Settings,
    looping: bool,
    metronome: bool,
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
        let config = EngineConfig { audio: Some(settings.audio.clone()), open_in_background: true, ..EngineConfig::default() };
        let (handle, banner) = match Engine::new(config, live.clone()) {
            Ok((h, _)) => {
                let banner = h.last_error().map(|e| no_device_banner(&e));
                (h, banner)
            }
            Err(e) => {
                // Could not even start the engine threads with a device:
                // run headless so editing and export keep working.
                let (h, _) = Engine::new(EngineConfig::headless(48_000.), live.clone()).expect("start the headless engine");
                (h, Some(no_device_banner(&e.to_string())))
            }
        };
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
        };
        this.adopt_active();
        this.refresh_devices();
        this
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

    fn locate(&mut self, tick: Tick) {
        self.handle.seek(tick.max(0));
        self.frame.transport.position = tick.max(0);
    }

    fn transport(&self) -> TransportState {
        TransportState { looping: self.looping, metronome: self.metronome, recording: self.recording, ..self.frame.transport }
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
        let session_rate = self.session_rate() as f64;
        let spawned = std::thread::Builder::new().name("aitrack-export".into()).spawn(move || {
            let range = match options.range {
                ExportRange::Song => RenderRange::Song,
                ExportRange::Loop if project.loop_region.is_some() => RenderRange::Loop,
                ExportRange::Loop => RenderRange::Song,
                ExportRange::Selection { start, end } => RenderRange::Ticks(start, end),
            };
            let opts = RenderOptions {
                range,
                session_rate,
                export_rate: options.sample_rate,
                block: 512,
                tail_seconds: options.tail_seconds.max(0.) as f64,
                normalize_db: options.normalize.then_some(-0.3),
                samples: None,
            };
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

    fn take_messages(&mut self) -> Vec<String> {
        let mut m = std::mem::take(&mut self.messages);
        m.extend(self.rt.take_messages());
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
