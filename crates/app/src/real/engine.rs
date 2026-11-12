//! [`EngineBridge`] over `aitrack-engine`.
//!
//! The project goes to the engine on every History revision change; the
//! engine compiles it off the audio thread. Transport, meters, CPU and clip
//! states are read from the handle's atomics once per frame ([`tick`]) and
//! cached, so the many views that ask during one paint all see the same
//! values. Export runs on its own thread with fresh processor instances.
//!
//! [`tick`]: EngineBridge::tick

use super::plugins::PluginRuntime;
use super::settings::Settings;
use crate::bridge::*;
use aitrack_engine::{
    ActiveAudio, ChannelSelection, ClipSlotStatus, Engine, EngineConfig, EngineError, EngineHandle, FactoryStack, Lame, Mp3Tags,
    RenderOptions, RenderRange, ShareMode, TapReader, WavFormat,
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
        let config = EngineConfig { audio: Some(settings.audio.clone()), ..EngineConfig::default() };
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
        };
        if let Some(a) = &this.active {
            this.app_config.sample_rate = a.sample_rate;
            if let Some(b) = a.buffer_size {
                this.app_config.buffer_size = b;
            }
            this.messages.extend(a.notes.iter().cloned());
        }
        this.refresh_devices();
        this
    }

    fn refresh_devices(&mut self) {
        let mut hosts = Vec::new();
        for h in self.handle.hosts() {
            hosts.extend_from_slice(hosts_for(&h));
        }
        self.hosts = hosts;
        let mut out = Vec::new();
        for d in self.handle.devices() {
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

    fn tick(&mut self) {
        let now = Instant::now();
        let dt = now.duration_since(self.frame.at).as_secs_f32().min(0.5);
        self.frame.at = now;
        let position = self.handle.position_ticks().max(0.) as Tick;
        self.frame.transport = TransportState { playing: self.handle.is_playing(), position, ..self.frame.transport };
        self.frame.cpu = self.handle.cpu_load().0.clamp(0., 1.);
        let fall = 10f32.powf(-METER_FALL_DB_S * dt / 20.);
        for &id in &self.tracks {
            let reading = if Some(id) == self.main_track { Some(self.handle.master_meter()) } else { self.handle.meters(id) };
            let peak = reading.map_or([0.; 2], |r| r.peak);
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
        // Nobody drained the tap for a while (window hidden or minimised):
        // stop feeding it until the next drain.
        if self.tap.is_enabled() && self.tap_drained.elapsed() > TAP_IDLE {
            self.tap.set_enabled(false);
        }
        self.rt.idle()
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
        self.tap.read(out);
        self.session_rate()
    }

    fn device_meter(&mut self, device: DeviceId, out: &mut [f32]) -> usize {
        self.handle.device_meter(device, out)
    }

    fn preview(&mut self, path: Option<&std::path::Path>) {
        if let Err(e) = self.preview.play(path) {
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
        let engine_cfg = to_engine(&config);
        match self.handle.set_device(Some(engine_cfg.clone())) {
            Ok(active) => {
                if let Some(a) = &active {
                    config.sample_rate = a.sample_rate;
                    if let Some(b) = a.buffer_size {
                        config.buffer_size = b;
                    }
                    self.messages.extend(a.notes.iter().cloned());
                }
                self.active = active;
                self.banner = None;
                self.app_config = config.clone();
                self.settings.audio = engine_cfg;
                self.settings.save();
                Ok(config)
            }
            Err(e) => {
                let msg = e.to_string();
                self.banner = Some(no_device_banner(&msg));
                self.app_config = config;
                Err(msg)
            }
        }
    }

    fn test_tone(&mut self) {
        self.messages.push("The test tone is not available in this engine build yet".into());
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

    fn status_label(&self) -> String {
        match &self.active {
            Some(a) => {
                let buf = a.buffer_size.map(|b| format!(" · {b}")).unwrap_or_default();
                format!("{} · {:.1} kHz{buf}", a.host, a.sample_rate as f32 / 1000.).replace(".0 kHz", " kHz")
            }
            None => "No audio device".into(),
        }
    }

    fn open_editor(&mut self, device: DeviceId) -> Result<(), String> {
        self.rt.open_editor(device)
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
}
