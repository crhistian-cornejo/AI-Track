//! The boundary between the interface and the rest of AI-Track.
//!
//! The UI talks to the audio engine, the plugin catalogue, the sample
//! library and the AI features only through these traits. Each has a stub
//! that returns demo data (and fakes a playing transport and moving meters),
//! so every view can be exercised without the real crates. The adapters over
//! the real crates live in `crate::real`; `AITRACK_STUBS=1` runs the app on
//! the stubs.

use aitrack_dsp::ParamInfo;
use aitrack_model::{Key, PluginFormat, Project, Scale, ScaleKind, SceneId, Tick, TrackId, TrackKind, PPQ};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

// ───────────────────────────── Engine ─────────────────────────────

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TransportState {
    pub playing: bool,
    pub recording: bool,
    pub looping: bool,
    pub metronome: bool,
    /// Song position in ticks.
    pub position: Tick,
}

/// Peak levels, linear 0..=1 (1 = 0 dBFS), already ballistically smoothed.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MeterLevel {
    pub left: f32,
    pub right: f32,
}

/// What a session slot is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotState {
    /// Playing since `since` (song ticks).
    Playing { scene: SceneId, since: Tick },
    /// Launched, waiting for the launch quantisation.
    Queued { scene: SceneId },
}

pub trait EngineBridge {
    fn play(&mut self);
    fn stop(&mut self);
    fn toggle_record(&mut self);
    fn set_looping(&mut self, on: bool);
    fn set_metronome(&mut self, on: bool);
    fn locate(&mut self, tick: Tick);
    fn transport(&self) -> TransportState;
    /// A new snapshot of the project after an edit (the engine compiles it off the audio thread).
    fn update_project(&mut self, project: &Project);
    /// Per-track peak meters, including the Main track.
    fn meter(&self, track: TrackId) -> MeterLevel;
    /// DSP load, 0..=1.
    fn cpu_load(&self) -> f32;
    fn launch_clip(&mut self, track: TrackId, scene: SceneId);
    fn launch_scene(&mut self, scene: SceneId);
    fn stop_track(&mut self, track: TrackId);
    fn slot_state(&self, track: TrackId) -> Option<SlotState>;
    fn audition_note(&mut self, _track: TrackId, _key: u8, _velocity: Option<u8>) -> bool {
        false
    }

    // ── Audio device (Preferences → Audio) ──
    fn audio_hosts(&self) -> Vec<AudioHost>;
    /// Every device on every host.
    fn audio_devices(&self) -> Vec<AudioDevice>;
    fn audio_config(&self) -> AudioConfig;
    /// Apply a configuration; the engine clamps what the device cannot do.
    fn set_audio_config(&mut self, config: AudioConfig) -> Result<AudioConfig, String>;
    /// A short 440 Hz tone on the selected output channels.
    fn test_tone(&mut self);

    // ── Offline export ──
    /// Render in the background; `progress` runs on the render thread and
    /// returns `false` to cancel.
    fn export(&mut self, options: ExportOptions, project: &Project, progress: ProgressFn);
    /// Why MP3 export cannot run (LAME missing), shown in the dialog.
    fn mp3_available(&self) -> Result<(), String> {
        Ok(())
    }

    // ── Per-frame and housekeeping (UI thread) ──
    /// Once per painted frame: read transport, meters, CPU and clip states.
    fn tick(&mut self) {}
    /// On a short timer even while idle: main-thread plugin work (loading,
    /// editor housekeeping). Returns true when the UI should repaint.
    fn idle(&mut self) -> bool {
        false
    }
    /// A non-blocking problem to show above the workspace (no audio device…).
    fn banner(&self) -> Option<String> {
        None
    }
    fn dismiss_banner(&mut self) {}
    /// Short status-bar text ("WASAPI · 48 kHz · 256").
    fn status_label(&self) -> String {
        "Engine: stub".into()
    }

    // ── Hosted plugins ──
    /// Open a VST3/CLAP device's editor window (main thread).
    fn open_editor(&mut self, _device: aitrack_model::DeviceId) -> Result<(), String> {
        Err("Plugin windows need the real engine".into())
    }
    /// Run third-party plugins out of process (applies to newly loaded instances).
    fn sandbox_plugins(&self) -> bool {
        false
    }
    fn set_sandbox_plugins(&mut self, _on: bool) {}
    /// Audition an audio file at a low level (`None` stops).
    fn preview(&mut self, _path: Option<&Path>) {}
    /// Plugin load failures and similar, drained by the UI as toasts.
    fn take_messages(&mut self) -> Vec<String> {
        Vec::new()
    }
}

/// Fake engine: the position follows the wall clock at the project tempo and
/// wraps in the loop; meters are a deterministic function of time.
pub struct StubEngine {
    state: TransportState,
    started: Option<(Instant, Tick)>,
    bpm: f64,
    loop_region: Option<(Tick, Tick)>,
    tracks: Vec<(TrackId, TrackKind, bool)>,
    slots: BTreeMap<TrackId, SlotState>,
    clip_slots: BTreeMap<TrackId, Vec<SceneId>>,
    audio: AudioConfig,
}

impl Default for StubEngine {
    fn default() -> Self {
        Self {
            state: TransportState { metronome: false, looping: true, ..Default::default() },
            started: None,
            bpm: 120.,
            loop_region: None,
            tracks: Vec::new(),
            slots: BTreeMap::new(),
            clip_slots: BTreeMap::new(),
            audio: AudioConfig::default(),
        }
    }
}

impl StubEngine {
    fn now_tick(&self) -> Tick {
        let Some((at, from)) = self.started else { return self.state.position };
        let elapsed = at.elapsed().as_secs_f64();
        let mut tick = from + (elapsed * self.bpm / 60. * PPQ as f64) as Tick;
        if self.state.looping
            && let Some((a, b)) = self.loop_region
                && b > a && tick >= b {
                    tick = a + (tick - a) % (b - a);
                }
        tick
    }

    fn bar(&self) -> Tick {
        4 * PPQ
    }

    fn next_bar(&self) -> Tick {
        let t = self.now_tick();
        (t / self.bar() + 1) * self.bar()
    }
}

impl EngineBridge for StubEngine {
    fn play(&mut self) {
        if !self.state.playing {
            self.state.playing = true;
            self.started = Some((Instant::now(), self.state.position));
            // Queued clips start immediately on play.
            let pos = self.state.position;
            for s in self.slots.values_mut() {
                if let SlotState::Queued { scene } = *s {
                    *s = SlotState::Playing { scene, since: pos };
                }
            }
        }
    }

    fn stop(&mut self) {
        if self.state.playing {
            self.state.position = self.now_tick();
            self.state.playing = false;
            self.state.recording = false;
            self.started = None;
        } else {
            // A second stop returns to the start and stops the clips (Ableton).
            self.state.position = 0;
            self.slots.clear();
        }
    }

    fn toggle_record(&mut self) {
        self.state.recording = !self.state.recording;
    }

    fn set_looping(&mut self, on: bool) {
        self.state.position = self.now_tick();
        if self.state.playing {
            self.started = Some((Instant::now(), self.state.position));
        }
        self.state.looping = on;
    }

    fn set_metronome(&mut self, on: bool) {
        self.state.metronome = on;
    }

    fn locate(&mut self, tick: Tick) {
        self.state.position = tick.max(0);
        if self.state.playing {
            self.started = Some((Instant::now(), self.state.position));
        }
    }

    fn transport(&self) -> TransportState {
        TransportState { position: self.now_tick(), ..self.state }
    }

    fn update_project(&mut self, project: &Project) {
        let bpm = project.tempo.initial_bpm();
        if (bpm - self.bpm).abs() > 1e-9 && self.state.playing {
            self.state.position = self.now_tick();
            self.started = Some((Instant::now(), self.state.position));
        }
        self.bpm = bpm;
        self.loop_region = project.loop_region;
        self.tracks = project.tracks.iter().map(|t| (t.id, t.kind, t.mute)).collect();
        self.clip_slots = project
            .tracks
            .iter()
            .map(|t| (t.id, t.session_slots.keys().copied().collect()))
            .collect();
    }

    fn meter(&self, track: TrackId) -> MeterLevel {
        if !self.state.playing {
            return MeterLevel::default();
        }
        let muted = self.tracks.iter().find(|(id, ..)| *id == track).is_none_or(|(_, kind, mute)| {
            *mute || matches!(kind, TrackKind::Folder | TrackKind::Vca)
        });
        if muted {
            return MeterLevel::default();
        }
        let tick = self.now_tick() as f32;
        let beat = tick / PPQ as f32;
        let seed = (track.0 % 7) as f32;
        // A kick-like envelope per beat plus a slower swell, different per track.
        let env = (-(beat.fract()) * (3. + seed * 0.6)).exp();
        let swell = 0.5 + 0.5 * (beat * 0.37 + seed).sin();
        let base = 0.35 + 0.25 * swell + 0.35 * env;
        let l = (base * (0.92 + 0.08 * (beat * 3.1 + seed).sin())).clamp(0., 1.);
        let r = (base * (0.92 + 0.08 * (beat * 2.7 + seed * 1.3).cos())).clamp(0., 1.);
        MeterLevel { left: l * 0.95, right: r * 0.95 }
    }

    fn cpu_load(&self) -> f32 {
        if self.state.playing {
            let t = self.now_tick() as f32 / PPQ as f32;
            0.18 + 0.04 * (t * 0.9).sin()
        } else {
            0.03
        }
    }

    fn launch_clip(&mut self, track: TrackId, scene: SceneId) {
        let state = if self.state.playing {
            // Quantised to the next bar; the stub starts it right away but
            // counts from the bar line so the progress ring stays in phase.
            let _ = self.next_bar();
            SlotState::Playing { scene, since: (self.now_tick() / self.bar()) * self.bar() }
        } else {
            SlotState::Queued { scene }
        };
        self.slots.insert(track, state);
    }

    fn launch_scene(&mut self, scene: SceneId) {
        let tracks: Vec<TrackId> = self
            .clip_slots
            .iter()
            .filter(|(_, scenes)| scenes.contains(&scene))
            .map(|(t, _)| *t)
            .collect();
        for t in tracks {
            self.launch_clip(t, scene);
        }
    }

    fn stop_track(&mut self, track: TrackId) {
        self.slots.remove(&track);
    }

    fn slot_state(&self, track: TrackId) -> Option<SlotState> {
        self.slots.get(&track).copied()
    }

    fn audio_hosts(&self) -> Vec<AudioHost> {
        if cfg!(target_os = "macos") {
            vec![AudioHost::CoreAudio]
        } else {
            vec![AudioHost::Wasapi, AudioHost::WasapiExclusive, AudioHost::Asio]
        }
    }

    fn audio_devices(&self) -> Vec<AudioDevice> {
        let dev = |id: &str, name: &str, host, i, o, rates: &[u32], range| AudioDevice {
            id: id.into(),
            name: name.into(),
            host,
            input_channels: i,
            output_channels: o,
            sample_rates: rates.to_vec(),
            buffer_range: range,
            default_rate: 48_000,
        };
        let all = &SAMPLE_RATES[..];
        let low = &SAMPLE_RATES[..4];
        let mut out = Vec::new();
        for host in self.audio_hosts() {
            match host {
                AudioHost::Asio => {
                    out.push(dev("asio-focusrite", "Focusrite USB ASIO", host, 2, 2, all, (32, 2048)));
                    out.push(dev("asio-generic", "Generic Low Latency ASIO", host, 2, 2, low, (64, 4096)));
                }
                AudioHost::CoreAudio => {
                    out.push(dev("ca-builtin", "MacBook Speakers", host, 0, 2, low, (32, 4096)));
                    out.push(dev("ca-mic", "MacBook Microphone", host, 1, 0, low, (32, 4096)));
                    out.push(dev("ca-interface", "Audio Interface", host, 8, 8, all, (32, 4096)));
                }
                _ => {
                    out.push(dev("wasapi-speakers", "Speakers (Realtek Audio)", host, 0, 2, &[44_100, 48_000], (128, 4096)));
                    out.push(dev("wasapi-mic", "Microphone (USB Audio)", host, 2, 0, &[44_100, 48_000], (128, 4096)));
                    out.push(dev("wasapi-interface", "Line (Audio Interface)", host, 4, 4, low, (64, 4096)));
                }
            }
        }
        out
    }

    fn audio_config(&self) -> AudioConfig {
        self.audio.clone()
    }

    fn set_audio_config(&mut self, mut config: AudioConfig) -> Result<AudioConfig, String> {
        let devices = self.audio_devices();
        let output = config.output.as_ref().and_then(|id| devices.iter().find(|d| &d.id == id && d.host == config.host));
        if let Some(d) = output {
            if !d.sample_rates.contains(&config.sample_rate) {
                config.sample_rate = d.default_rate;
            }
            config.buffer_size = config.buffer_size.clamp(d.buffer_range.0, d.buffer_range.1);
            config.output_channels.retain(|c| *c < d.output_channels);
        }
        self.audio = config.clone();
        Ok(config)
    }

    fn test_tone(&mut self) {}

    fn export(&mut self, options: ExportOptions, _project: &Project, progress: ProgressFn) {
        stub_export(options, progress);
    }
}


// ───────────────────────────── Audio device & export ─────────────────────────────

/// Audio driver / host API.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AudioHost {
    Wasapi,
    WasapiExclusive,
    Asio,
    CoreAudio,
}

impl AudioHost {
    pub fn label(self) -> &'static str {
        match self {
            Self::Wasapi => "WASAPI (shared)",
            Self::WasapiExclusive => "WASAPI Exclusive",
            Self::Asio => "ASIO",
            Self::CoreAudio => "CoreAudio",
        }
    }
}

/// The sample rates the UI offers (a device lists the subset it supports).
pub const SAMPLE_RATES: [u32; 6] = [44_100, 48_000, 88_200, 96_000, 176_400, 192_000];
/// Buffer sizes the UI offers (clamped to each device's range).
pub const BUFFER_SIZES: [u32; 8] = [32, 64, 128, 256, 512, 1024, 2048, 4096];

#[derive(Clone, Debug, PartialEq)]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
    pub host: AudioHost,
    pub input_channels: u16,
    pub output_channels: u16,
    pub sample_rates: Vec<u32>,
    pub buffer_range: (u32, u32),
    pub default_rate: u32,
}

/// What the user picks in Preferences → Audio. `None` devices mean "default".
#[derive(Clone, Debug, PartialEq)]
pub struct AudioConfig {
    pub host: AudioHost,
    pub input: Option<String>,
    pub output: Option<String>,
    pub sample_rate: u32,
    pub buffer_size: u32,
    /// Enabled channels, 0-based.
    pub input_channels: Vec<u16>,
    pub output_channels: Vec<u16>,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            host: if cfg!(target_os = "macos") { AudioHost::CoreAudio } else { AudioHost::Wasapi },
            input: None,
            output: None,
            sample_rate: 48_000,
            buffer_size: 256,
            input_channels: vec![0, 1],
            output_channels: vec![0, 1],
        }
    }
}

impl AudioConfig {
    /// One-way buffer latency in milliseconds.
    pub fn buffer_ms(&self) -> f32 {
        self.buffer_size as f32 / self.sample_rate.max(1) as f32 * 1000.
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WavDepth {
    /// 16-bit integer with TPDF dither (always on at 16-bit).
    Int16Dithered,
    Int24,
    Float32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    Wav(WavDepth),
    /// Always 320 kbps CBR.
    Mp3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportRange {
    Song,
    Loop,
    Selection { start: Tick, end: Tick },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExportMode {
    Mixdown,
    /// One WAV per track/group/return.
    Stems { tracks: Vec<TrackId> },
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExportOptions {
    pub format: ExportFormat,
    pub sample_rate: u32,
    pub range: ExportRange,
    pub mode: ExportMode,
    /// Peak-normalise to -0.3 dBFS (one shared gain for stems).
    pub normalize: bool,
    /// Rendered past the end so reverbs and delays ring out.
    pub tail_seconds: f32,
    pub folder: PathBuf,
    pub file_name: String,
}

impl ExportOptions {
    /// Stems are always WAV; MP3 only exists for mixdowns.
    pub fn validate(&self) -> Result<(), &'static str> {
        if matches!(self.mode, ExportMode::Stems { .. }) && self.format == ExportFormat::Mp3 {
            return Err("Stems are always exported as WAV");
        }
        if let ExportMode::Stems { tracks } = &self.mode
            && tracks.is_empty() {
                return Err("Choose at least one track for stems");
            }
        if let ExportRange::Selection { start, end } = self.range
            && end <= start {
                return Err("The selection is empty");
            }
        if self.file_name.trim().is_empty() {
            return Err("Enter a file name");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExportProgress {
    pub fraction: f32,
    pub stage: String,
    pub done: bool,
    pub cancelled: bool,
    pub error: Option<String>,
    pub files: Vec<PathBuf>,
}

/// Called from the render thread; return `false` to cancel.
pub type ProgressFn = Box<dyn FnMut(ExportProgress) -> bool + Send>;

/// The stub "renders" on a thread for a couple of seconds and reports progress.
fn stub_export(options: ExportOptions, mut progress: ProgressFn) {
    std::thread::spawn(move || {
        let names: Vec<String> = match &options.mode {
            ExportMode::Mixdown => vec![options.file_name.clone()],
            ExportMode::Stems { tracks } => tracks.iter().map(|t| format!("{} - track {}", options.file_name, t.0)).collect(),
        };
        let ext = if options.format == ExportFormat::Mp3 { "mp3" } else { "wav" };
        let steps = 40;
        for i in 0..=steps {
            let f = i as f32 / steps as f32;
            let stage = if f < 0.85 { "Rendering" } else if options.normalize && f < 0.95 { "Normalizing" } else { "Writing files" };
            let keep = progress(ExportProgress { fraction: f, stage: stage.into(), done: false, cancelled: false, error: None, files: Vec::new() });
            if !keep {
                progress(ExportProgress { fraction: f, stage: "Cancelled".into(), done: true, cancelled: true, error: None, files: Vec::new() });
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let files = names.iter().map(|n| options.folder.join(format!("{n}.{ext}"))).collect();
        progress(ExportProgress { fraction: 1., stage: "Done".into(), done: true, cancelled: false, error: None, files });
    });
}

// ───────────────────────────── Plugins ─────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum DeviceCategory {
    Instrument,
    Drums,
    Dynamics,
    Eq,
    Filter,
    Space,
    Distortion,
    Modulation,
    Utility,
    Rack,
}

impl DeviceCategory {
    pub fn label(self) -> &'static str {
        match self {
            Self::Instrument => "Instruments",
            Self::Drums => "Drums",
            Self::Dynamics => "Dynamics",
            Self::Eq => "EQ",
            Self::Filter => "Filter",
            Self::Space => "Reverb & Delay",
            Self::Distortion => "Distortion",
            Self::Modulation => "Modulation",
            Self::Utility => "Utility",
            Self::Rack => "Racks",
        }
    }
}

#[derive(Clone, Debug)]
pub struct DeviceInfo {
    /// Stock plugin id, as in `DeviceKind::Stock { plugin_id }` (e.g. `"fx.eq"`).
    pub id: &'static str,
    pub name: &'static str,
    pub category: DeviceCategory,
    pub tags: &'static [&'static str],
    pub params: Vec<ParamInfo>,
    pub is_instrument: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExternalPlugin {
    pub uid: String,
    pub name: String,
    pub vendor: String,
    pub format: PluginFormat,
    pub path: PathBuf,
    pub is_instrument: bool,
}

/// A factory preset of a stock device.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PresetInfo {
    /// Factory path, `plugin/Category/Name.json`.
    pub path: String,
    pub category: String,
    pub name: String,
}

/// Plugin folder scan progress.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScanStatus {
    pub running: bool,
    pub fraction: f32,
    pub message: String,
}

pub trait PluginCatalog {
    fn stock_devices(&self) -> &[DeviceInfo];
    fn external_plugins(&self) -> &[ExternalPlugin];
    fn device(&self, id: &str) -> Option<&DeviceInfo> {
        self.stock_devices().iter().find(|d| d.id == id)
    }
    /// Factory presets of a stock device, sorted by category then name.
    fn presets(&self, _plugin_id: &str) -> Vec<PresetInfo> {
        Vec::new()
    }
    /// A preset's display name and normalised parameter values.
    fn load_preset(&self, path: &str) -> Result<(String, BTreeMap<u32, f32>), String> {
        Err(format!("no preset {path}"))
    }
    /// Scan the VST3/CLAP folders again in the background.
    fn rescan(&mut self) {}
    fn scan_status(&self) -> ScanStatus {
        ScanStatus::default()
    }
    /// Pick up background scan results; true when the list changed.
    fn poll(&mut self) -> bool {
        false
    }
}

pub struct StubCatalog {
    stock: Vec<DeviceInfo>,
    external: Vec<ExternalPlugin>,
}

const WAVES: &[&str] = &["Saw", "Square", "Sine", "Wavetable"];
const FILTERS: &[&str] = &["LP 24", "LP 12", "BP", "HP"];

fn p(id: u32, name: &'static str, min: f32, max: f32, default: f32) -> ParamInfo {
    ParamInfo::new(id, name, min, max, default)
}

impl Default for StubCatalog {
    fn default() -> Self {
        use DeviceCategory::*;
        let synth = vec![
            p(0, "Osc A", 0., 3., 0.).short("Osc A").choices(WAVES),
            p(1, "Osc B", 0., 3., 1.).short("Osc B").choices(WAVES),
            p(2, "Detune", 0., 100., 12.).unit("ct").short("Detune"),
            p(3, "Cutoff", 20., 20_000., 2_400.).unit("Hz").log().short("Cutoff"),
            p(4, "Resonance", 0., 100., 18.).unit("%").short("Reso"),
            p(5, "Filter", 0., 3., 0.).choices(FILTERS),
            p(6, "Env Amount", -100., 100., 35.).unit("%").short("Env"),
            p(7, "Attack", 0.1, 5_000., 4.).unit("ms").log().short("Attack"),
            p(8, "Decay", 1., 5_000., 320.).unit("ms").log().short("Decay"),
            p(9, "Sustain", 0., 100., 60.).unit("%").short("Sustain"),
            p(10, "Release", 1., 8_000., 280.).unit("ms").log().short("Release"),
            p(11, "Volume", -60., 6., -6.).unit("dB").short("Volume"),
        ];
        let sampler = vec![
            p(0, "Start", 0., 100., 0.).unit("%"),
            p(1, "Transpose", -24., 24., 0.).unit("st").short("Pitch"),
            p(2, "Attack", 0.1, 2_000., 1.).unit("ms").log(),
            p(3, "Release", 1., 4_000., 120.).unit("ms").log(),
            p(4, "Cutoff", 20., 20_000., 20_000.).unit("Hz").log(),
            p(5, "Volume", -60., 6., 0.).unit("dB"),
        ];
        let eq = vec![
            p(0, "Low", -18., 18., 0.).unit("dB"),
            p(1, "Low Freq", 20., 500., 90.).unit("Hz").log().short("L Freq"),
            p(2, "Mid", -18., 18., 0.).unit("dB"),
            p(3, "Mid Freq", 200., 8_000., 1_200.).unit("Hz").log().short("M Freq"),
            p(4, "High", -18., 18., 0.).unit("dB"),
            p(5, "High Freq", 2_000., 20_000., 8_000.).unit("Hz").log().short("H Freq"),
            p(6, "Output", -18., 18., 0.).unit("dB"),
        ];
        let comp = vec![
            p(0, "Threshold", -60., 0., -18.).unit("dB").short("Thresh"),
            p(1, "Ratio", 1., 20., 4.).power(0.5),
            p(2, "Attack", 0.05, 200., 10.).unit("ms").log(),
            p(3, "Release", 5., 2_000., 120.).unit("ms").log(),
            p(4, "Knee", 0., 24., 6.).unit("dB"),
            p(5, "Makeup", 0., 24., 0.).unit("dB"),
            p(6, "Mix", 0., 100., 100.).unit("%"),
        ];
        let limiter = vec![
            p(0, "Ceiling", -12., 0., -0.3).unit("dB"),
            p(1, "Gain", 0., 24., 0.).unit("dB"),
            p(2, "Release", 1., 1_000., 60.).unit("ms").log(),
            p(3, "Lookahead", 0.5, 5., 1.5).unit("ms").short("Look"),
        ];
        let reverb = vec![
            p(0, "Size", 0., 100., 55.).unit("%"),
            p(1, "Decay", 100., 20_000., 2_400.).unit("ms").log(),
            p(2, "Pre-delay", 0., 250., 18.).unit("ms").short("Pre"),
            p(3, "Damping", 0., 100., 40.).unit("%").short("Damp"),
            p(4, "Width", 0., 100., 100.).unit("%"),
            p(5, "Mix", 0., 100., 30.).unit("%"),
        ];
        let delay = vec![
            p(0, "Time", 1., 2_000., 375.).unit("ms").log(),
            p(1, "Feedback", 0., 100., 38.).unit("%").short("Fdbk"),
            p(2, "Low Cut", 20., 2_000., 200.).unit("Hz").log().short("Lo Cut"),
            p(3, "High Cut", 1_000., 20_000., 8_000.).unit("Hz").log().short("Hi Cut"),
            p(4, "Ping-Pong", 0., 1., 1.).choices(&["Off", "On"]).short("Ping"),
            p(5, "Mix", 0., 100., 25.).unit("%"),
        ];
        let saturator = vec![
            p(0, "Drive", 0., 36., 6.).unit("dB"),
            p(1, "Shape", 0., 3., 0.).choices(&["Soft", "Tape", "Tube", "Fold"]),
            p(2, "Tone", -100., 100., 0.).unit("%"),
            p(3, "Output", -24., 6., 0.).unit("dB"),
            p(4, "Mix", 0., 100., 100.).unit("%"),
        ];
        let filter = vec![
            p(0, "Cutoff", 20., 20_000., 1_000.).unit("Hz").log(),
            p(1, "Resonance", 0., 100., 20.).unit("%").short("Reso"),
            p(2, "Type", 0., 3., 0.).choices(FILTERS),
            p(3, "Drive", 0., 24., 0.).unit("dB"),
            p(4, "Mix", 0., 100., 100.).unit("%"),
        ];
        let chorus = vec![
            p(0, "Rate", 0.01, 10., 0.6).unit("Hz").log(),
            p(1, "Depth", 0., 100., 40.).unit("%"),
            p(2, "Feedback", 0., 100., 10.).unit("%").short("Fdbk"),
            p(3, "Mix", 0., 100., 50.).unit("%"),
        ];
        let utility = vec![
            p(0, "Gain", -36., 36., 0.).unit("dB"),
            p(1, "Width", 0., 200., 100.).unit("%"),
            p(2, "Pan", -100., 100., 0.).unit("%"),
            p(3, "Mono", 0., 1., 0.).choices(&["Off", "On"]),
        ];
        let stock = vec![
            DeviceInfo { id: "instrument.wavetable", name: "Wavetable Synth", category: Instrument, tags: &["synth", "poly", "lead", "pad"], params: synth.clone(), is_instrument: true },
            DeviceInfo { id: "instrument.bass", name: "Mono Bass", category: Instrument, tags: &["synth", "mono", "bass"], params: synth, is_instrument: true },
            DeviceInfo { id: "instrument.sampler", name: "Sampler", category: Instrument, tags: &["sampler", "keys"], params: sampler.clone(), is_instrument: true },
            DeviceInfo { id: "instrument.drum_sampler", name: "Drum Sampler", category: Drums, tags: &["drums", "one-shot"], params: sampler, is_instrument: true },
            DeviceInfo { id: "fx.eq", name: "EQ Eight", category: Eq, tags: &["eq", "tone"], params: eq, is_instrument: false },
            DeviceInfo { id: "fx.compressor", name: "Compressor", category: Dynamics, tags: &["dynamics", "glue"], params: comp, is_instrument: false },
            DeviceInfo { id: "fx.limiter", name: "Limiter", category: Dynamics, tags: &["master", "loudness"], params: limiter, is_instrument: false },
            DeviceInfo { id: "fx.reverb", name: "Reverb", category: Space, tags: &["space", "hall", "plate"], params: reverb, is_instrument: false },
            DeviceInfo { id: "fx.delay", name: "Echo Delay", category: Space, tags: &["delay", "echo"], params: delay, is_instrument: false },
            DeviceInfo { id: "fx.saturator", name: "Saturator", category: Distortion, tags: &["drive", "warmth"], params: saturator, is_instrument: false },
            DeviceInfo { id: "fx.filter", name: "Auto Filter", category: Filter, tags: &["filter", "sweep"], params: filter, is_instrument: false },
            DeviceInfo { id: "fx.chorus", name: "Chorus", category: Modulation, tags: &["width", "chorus"], params: chorus, is_instrument: false },
            DeviceInfo { id: "fx.utility", name: "Utility", category: Utility, tags: &["gain", "width"], params: utility, is_instrument: false },
            DeviceInfo { id: "rack.instrument", name: "Instrument Rack", category: Rack, tags: &["rack", "macros"], params: Vec::new(), is_instrument: true },
            DeviceInfo { id: "rack.drum", name: "Drum Rack", category: Rack, tags: &["rack", "drums", "pads"], params: Vec::new(), is_instrument: true },
            DeviceInfo { id: "rack.audio", name: "Audio Effect Rack", category: Rack, tags: &["rack", "macros"], params: Vec::new(), is_instrument: false },
        ];
        let ext = |name: &str, vendor: &str, format, inst| ExternalPlugin {
            uid: format!("{vendor}.{name}").to_lowercase().replace(' ', "-"),
            name: name.into(),
            vendor: vendor.into(),
            format,
            path: PathBuf::from(match format {
                PluginFormat::Vst3 => format!("C:/Program Files/Common Files/VST3/{name}.vst3"),
                PluginFormat::Clap => format!("C:/Program Files/Common Files/CLAP/{name}.clap"),
            }),
            is_instrument: inst,
        };
        let external = vec![
            ext("Surge XT", "Surge Synth Team", PluginFormat::Clap, true),
            ext("Vital", "Vital Audio", PluginFormat::Vst3, true),
            ext("Dexed", "Digital Suburban", PluginFormat::Vst3, true),
            ext("OTT", "Xfer Records", PluginFormat::Vst3, false),
            ext("Valhalla Supermassive", "Valhalla DSP", PluginFormat::Vst3, false),
            ext("Airwindows Consolidated", "Airwindows", PluginFormat::Clap, false),
            ext("Chow Tape Model", "Chowdhury DSP", PluginFormat::Clap, false),
        ];
        Self { stock, external }
    }
}

impl PluginCatalog for StubCatalog {
    fn stock_devices(&self) -> &[DeviceInfo] {
        &self.stock
    }
    fn external_plugins(&self) -> &[ExternalPlugin] {
        &self.external
    }
}

// ───────────────────────────── Library ─────────────────────────────

#[derive(Clone, Debug, PartialEq)]
pub struct LibraryFolder {
    /// Stable path-like id, e.g. `"categories/drums/kicks"`.
    pub id: String,
    pub name: String,
    /// Samples inside (0 when unknown).
    pub count: u64,
    pub children: Vec<LibraryFolder>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LibraryItem {
    pub id: u64,
    pub name: String,
    pub path: PathBuf,
    /// e.g. "Kick", "Bass Loop", "Vocal".
    pub category: String,
    pub key: Option<Scale>,
    pub bpm: Option<f32>,
    /// Seconds.
    pub duration: f32,
    pub is_loop: bool,
    pub favorite: bool,
}

/// A Content Hub entry (or a bundled pack) as the hub page lists it.
#[derive(Clone, Debug, PartialEq)]
pub struct HubItem {
    pub name: String,
    pub author: String,
    /// "Sample pack", "Instrument", "Plugin" or "Included".
    pub kind: String,
    pub license: String,
    pub license_summary: String,
    pub redistributable: bool,
    pub url: String,
    pub description: String,
}

/// What "Organize on disk" would do, shown before anything moves.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OrganizePreview {
    pub dest: PathBuf,
    pub files: usize,
    pub bytes: u64,
    pub duplicates: usize,
    pub moves: bool,
    /// Target folder → number of files (largest first, a few).
    pub folders: Vec<(String, usize)>,
}

pub trait LibraryBridge {
    /// Virtual folders (Categories, Keys, Genres…), top level first.
    fn folders(&self) -> &[LibraryFolder];
    /// Items in a virtual folder (and its sub-folders).
    fn items(&self, folder: &str) -> Vec<LibraryItem>;
    /// Free-text search, optionally restricted to a key (compatible keys first).
    fn search(&self, query: &str, key: Option<Scale>) -> Vec<LibraryItem>;
    /// Samples ranked for the project's key and tempo.
    fn recommend(&self, scale: Option<Scale>, _bpm: f64) -> Vec<LibraryItem> {
        self.search("", scale)
    }
    /// Where the library comes from and how big it is.
    fn status(&self) -> String {
        "Demo library".into()
    }
    /// Index a folder into the user library (scanned in the background).
    fn add_folder(&mut self, _path: &Path) -> Result<(), String> {
        Err("The demo library cannot index folders".into())
    }
    fn rescan(&mut self) {}
    fn cancel_scan(&mut self) {}
    fn scan_status(&self) -> ScanStatus {
        ScanStatus::default()
    }
    /// Pick up scan progress and results; true when the UI should repaint.
    fn poll(&mut self) -> bool {
        false
    }
    fn set_favorite(&mut self, _id: u64, _on: bool) {}
    fn mark_used(&mut self, _id: u64) {}
    /// Content Hub entries and the bundled packs.
    fn hub(&self) -> Vec<HubItem> {
        Vec::new()
    }
    /// Plan copying (or moving) `items` into `dest`, by key or by category.
    fn plan_organize(&mut self, _items: &[u64], _dest: &Path, _by_key: bool, _move_files: bool) -> Result<OrganizePreview, String> {
        Err("Organizing needs the real library".into())
    }
    /// Run the last plan in the background; the result arrives through `organize_result`.
    fn execute_organize(&mut self) -> Result<(), String> {
        Err("Nothing planned".into())
    }
    /// A finished organize run (`Ok(summary)`), taken once.
    fn organize_result(&mut self) -> Option<Result<String, String>> {
        None
    }
    fn can_undo_organize(&self) -> bool {
        false
    }
    fn undo_organize(&mut self) -> Result<String, String> {
        Err("Nothing to undo".into())
    }
}

pub struct StubLibrary {
    folders: Vec<LibraryFolder>,
    items: Vec<(Vec<String>, LibraryItem)>,
}

fn folder(id: &str, name: &str, children: Vec<LibraryFolder>) -> LibraryFolder {
    LibraryFolder { id: id.into(), name: name.into(), count: 0, children }
}

impl Default for StubLibrary {
    fn default() -> Self {
        let cats = [
            ("drums", "Drums", &["Kicks", "Snares", "Claps", "Hats", "Percussion"][..]),
            ("bass", "Bass", &["One-shots", "Loops"][..]),
            ("synths", "Synths", &["Leads", "Pads", "Plucks"][..]),
            ("vocals", "Vocals", &["Chops", "Phrases"][..]),
            ("fx", "FX", &["Risers", "Impacts", "Atmos"][..]),
        ];
        let categories = folder(
            "categories",
            "Categories",
            cats.iter()
                .map(|(id, name, subs)| {
                    folder(
                        &format!("categories/{id}"),
                        name,
                        subs.iter()
                            .map(|s| folder(&format!("categories/{id}/{}", s.to_lowercase()), s, vec![]))
                            .collect(),
                    )
                })
                .collect(),
        );
        let key_names = ["A minor", "C major", "D minor", "E minor", "F major", "G minor"];
        let keys = folder(
            "keys",
            "Keys",
            key_names.iter().map(|k| folder(&format!("keys/{}", k.replace(' ', "-").to_lowercase()), k, vec![])).collect(),
        );
        let genres = folder(
            "genres",
            "Genres",
            ["House", "Techno", "Pop", "Reggaeton", "Drum & Bass", "Lo-fi"]
                .iter()
                .map(|g| folder(&format!("genres/{}", g.to_lowercase().replace([' ', '&'], "")), g, vec![]))
                .collect(),
        );
        let folders = vec![categories, keys, genres];

        let am = Scale::new(Key(9), ScaleKind::Minor);
        let cmaj = Scale::new(Key(0), ScaleKind::Major);
        let dm = Scale::new(Key(2), ScaleKind::Minor);
        let em = Scale::new(Key(4), ScaleKind::Minor);
        let fmaj = Scale::new(Key(5), ScaleKind::Major);
        let gm = Scale::new(Key(7), ScaleKind::Minor);
        type Row = (&'static str, &'static str, &'static str, Option<Scale>, Option<f32>, f32, bool, &'static str);
        let rows: &[Row] = &[
            ("Kick Punchy 01", "drums/kicks", "Kick", Some(am), None, 0.42, false, "house"),
            ("Kick Deep Sub", "drums/kicks", "Kick", Some(dm), None, 0.61, false, "techno"),
            ("Kick Analog 909", "drums/kicks", "Kick", None, None, 0.38, false, "techno"),
            ("Kick Dark Rumble", "drums/kicks", "Kick", Some(am), None, 0.88, false, "techno"),
            ("Kick Reggaeton Thump", "drums/kicks", "Kick", Some(gm), None, 0.45, false, "reggaeton"),
            ("Snare Crack", "drums/snares", "Snare", None, None, 0.31, false, "pop"),
            ("Snare Room Tight", "drums/snares", "Snare", None, None, 0.27, false, "house"),
            ("Clap Wide Stack", "drums/claps", "Clap", None, None, 0.35, false, "house"),
            ("Clap Vinyl", "drums/claps", "Clap", None, None, 0.29, false, "lofi"),
            ("Hat Closed Tick", "drums/hats", "Hat", None, None, 0.08, false, "techno"),
            ("Hat Open Sizzle", "drums/hats", "Hat", None, None, 0.52, false, "house"),
            ("Hat Loop 16th", "drums/hats", "Hat Loop", None, Some(128.), 7.5, true, "house"),
            ("Perc Dembow Loop", "drums/percussion", "Perc Loop", None, Some(95.), 10.1, true, "reggaeton"),
            ("Shaker Groove", "drums/percussion", "Perc Loop", None, Some(124.), 7.74, true, "house"),
            ("Bass Reese A", "bass/one-shots", "Bass", Some(am), None, 1.6, false, "drumbass"),
            ("Bass Sub Pluck", "bass/one-shots", "Bass", Some(em), None, 0.9, false, "pop"),
            ("Bass Rolling Loop Am", "bass/loops", "Bass Loop", Some(am), Some(128.), 7.5, true, "techno"),
            ("Bass Offbeat Loop Dm", "bass/loops", "Bass Loop", Some(dm), Some(124.), 7.74, true, "house"),
            ("Lead Supersaw Am", "synths/leads", "Lead", Some(am), Some(128.), 3.75, true, "house"),
            ("Lead Square Hook", "synths/leads", "Lead", Some(cmaj), Some(120.), 4.0, true, "pop"),
            ("Pad Warm Analog Am", "synths/pads", "Pad", Some(am), None, 6.2, false, "lofi"),
            ("Pad Glass F", "synths/pads", "Pad", Some(fmaj), None, 8.0, false, "pop"),
            ("Pluck Bell C", "synths/plucks", "Pluck", Some(cmaj), None, 1.1, false, "pop"),
            ("Pluck Chord Loop Am", "synths/plucks", "Pluck Loop", Some(am), Some(128.), 7.5, true, "house"),
            ("Vocal Chop Hey", "vocals/chops", "Vocal", Some(am), None, 0.6, false, "house"),
            ("Vocal Chop Ooh Gm", "vocals/chops", "Vocal", Some(gm), None, 1.2, false, "reggaeton"),
            ("Vocal Phrase Night", "vocals/phrases", "Vocal Loop", Some(am), Some(128.), 7.5, true, "pop"),
            ("Riser White 8 Bars", "fx/risers", "Riser", None, Some(128.), 15.0, true, "techno"),
            ("Impact Low Boom", "fx/impacts", "Impact", Some(am), None, 3.2, false, "techno"),
            ("Atmos Rain Tape", "fx/atmos", "Atmos", None, None, 12.0, false, "lofi"),
        ];
        let items = rows
            .iter()
            .enumerate()
            .map(|(i, (name, cat, category, key, bpm, dur, is_loop, genre))| {
                let mut tags = vec![format!("categories/{cat}"), format!("genres/{genre}")];
                if let Some(k) = key {
                    tags.push(format!("keys/{}", k.display().replace(' ', "-").to_lowercase()));
                }
                (
                    tags,
                    LibraryItem {
                        id: i as u64 + 1,
                        name: (*name).into(),
                        path: PathBuf::from(format!("Library/{cat}/{name}.wav")),
                        category: (*category).into(),
                        key: *key,
                        bpm: *bpm,
                        duration: *dur,
                        is_loop: *is_loop,
                        favorite: false,
                    },
                )
            })
            .collect();
        Self { folders, items }
    }
}

impl LibraryBridge for StubLibrary {
    fn folders(&self) -> &[LibraryFolder] {
        &self.folders
    }

    fn items(&self, folder: &str) -> Vec<LibraryItem> {
        self.items
            .iter()
            .filter(|(tags, _)| tags.iter().any(|t| t == folder || t.starts_with(&format!("{folder}/"))))
            .map(|(_, item)| item.clone())
            .collect()
    }

    fn search(&self, query: &str, key: Option<Scale>) -> Vec<LibraryItem> {
        let q = query.trim().to_lowercase();
        let mut out: Vec<LibraryItem> = self
            .items
            .iter()
            .map(|(_, i)| i)
            .filter(|i| {
                q.is_empty() || i.name.to_lowercase().contains(&q) || i.category.to_lowercase().contains(&q)
            })
            .filter(|i| key.is_none_or(|k| i.key.is_none_or(|ik| compatible(ik, k))))
            .cloned()
            .collect();
        // Exact key matches first.
        out.sort_by_key(|i| (i.key != key, i.name.clone()));
        out
    }
}

/// Same key, or the relative major/minor.
fn compatible(a: Scale, b: Scale) -> bool {
    a.mask() == b.mask()
}

// ───────────────────────────── AI ─────────────────────────────

pub type JobId = u64;

/// A note detected from audio, before it gets project ids.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DetectedNote {
    pub key: u8,
    pub start: Tick,
    pub length: Tick,
    pub velocity: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AiEvent {
    Progress { job: JobId, label: String, fraction: f32 },
    MidiReady { job: JobId, source_name: String, notes: Vec<DetectedNote> },
    StemsReady { job: JobId, stems: Vec<(String, PathBuf)> },
    /// A chat reply from the connected assistant.
    Reply { text: String },
    /// The assistant wants to edit the project; the UI stages it with
    /// `History::propose` so the user can accept or discard it.
    Proposal { label: String, agent: String, commands: Vec<aitrack_model::Command> },
    Failed { job: JobId, message: String },
    Analyzed { job: JobId, path: PathBuf, analysis: AudioAnalysis },
}

/// Result of analysing an audio file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AudioAnalysis {
    pub key: Option<Scale>,
    pub bpm: Option<f64>,
    pub sample_rate: u32,
    pub channels: u16,
    pub frames: u64,
}

/// Which audio-to-MIDI engine to run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MidiMode {
    /// basic-pitch (polyphonic, downloads a small model).
    #[default]
    Polyphonic,
    Monophonic,
    Drums,
}

impl MidiMode {
    pub const ALL: [MidiMode; 3] = [MidiMode::Polyphonic, MidiMode::Monophonic, MidiMode::Drums];
    pub fn label(self) -> &'static str {
        match self {
            Self::Polyphonic => "Poly",
            Self::Monophonic => "Mono",
            Self::Drums => "Drums",
        }
    }
}

/// What the user must see and accept before a model is downloaded.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelNotice {
    pub model: String,
    pub name: String,
    /// Download size in bytes, when known.
    pub size: Option<u64>,
    pub license: String,
    pub source: String,
    pub notice: Option<String>,
}

pub trait AiBridge {
    /// Audio to MIDI; notes are timed at `bpm`.
    fn extract_midi(&mut self, path: &Path, mode: MidiMode, bpm: f64) -> JobId;
    fn split_stems(&mut self, path: &Path) -> JobId;
    /// The stem model's licence notice while it still has to be accepted.
    fn stem_license(&self) -> Option<ModelNotice> {
        None
    }
    fn accept_license(&mut self, _model: &str) -> Result<(), String> {
        Ok(())
    }
    /// Key / BPM / format analysis of an audio file.
    fn analyze(&mut self, path: &Path) -> JobId;
    /// Send a chat message; replies and proposals arrive through [`poll`](Self::poll).
    fn chat(&mut self, text: &str, project: &Project);
    /// Drain finished work and progress. The UI calls this on a timer only
    /// while [`busy`](Self::busy) is true.
    fn poll(&mut self) -> Vec<AiEvent>;
    fn busy(&self) -> bool;
}

struct StubJob {
    id: JobId,
    kind: StubJobKind,
    started: Instant,
    reported: f32,
}

enum StubJobKind {
    Midi(String),
    Stems(PathBuf),
    /// The reply text and, when the message names a track, a proposal.
    Chat(String, Option<(String, Vec<aitrack_model::Command>)>),
    Fail(String),
    Analyze(PathBuf),
}

#[derive(Default)]
pub struct StubAi {
    jobs: Vec<StubJob>,
    next: JobId,
}

impl StubAi {
    fn push(&mut self, kind: StubJobKind) -> JobId {
        self.next += 1;
        self.jobs.push(StubJob { id: self.next, kind, started: Instant::now(), reported: 0. });
        self.next
    }
}

impl AiBridge for StubAi {
    fn analyze(&mut self, path: &Path) -> JobId {
        self.push(StubJobKind::Analyze(path.to_path_buf()))
    }

    fn extract_midi(&mut self, path: &Path, _mode: MidiMode, _bpm: f64) -> JobId {
        match path.file_stem() {
            Some(s) => self.push(StubJobKind::Midi(s.to_string_lossy().into_owned())),
            None => self.push(StubJobKind::Fail("no audio file".into())),
        }
    }

    fn split_stems(&mut self, path: &Path) -> JobId {
        if path.as_os_str().is_empty() {
            return self.push(StubJobKind::Fail("no audio file".into()));
        }
        self.push(StubJobKind::Stems(path.to_path_buf()))
    }

    fn chat(&mut self, text: &str, project: &Project) {
        // The stub "understands" one thing: a message naming a track proposes
        // nudging its level up, so the proposal flow can be exercised.
        let lower = text.to_lowercase();
        let proposal = project.content_tracks().find(|t| lower.contains(&t.name.to_lowercase())).map(|t| {
            let db = (t.volume_db + 1.5).min(6.);
            (
                format!("Raise {} by 1.5 dB", t.name),
                vec![aitrack_model::Command::SetTrack { id: t.id, change: aitrack_model::TrackProp::VolumeDb(db) }],
            )
        });
        self.push(StubJobKind::Chat(text.to_owned(), proposal));
    }

    fn poll(&mut self) -> Vec<AiEvent> {
        let mut out = Vec::new();
        let mut finished = Vec::new();
        for job in &mut self.jobs {
            let secs = job.started.elapsed().as_secs_f32();
            let total = match job.kind {
                StubJobKind::Midi(_) => 1.6,
                StubJobKind::Stems(_) => 3.2,
                StubJobKind::Chat(..) => 0.9,
                StubJobKind::Fail(_) | StubJobKind::Analyze(_) => 0.3,
            };
            let f = (secs / total).min(1.);
            if f < 1. {
                if f - job.reported >= 0.04 && matches!(job.kind, StubJobKind::Midi(_) | StubJobKind::Stems(_)) {
                    job.reported = f;
                    let label = match job.kind {
                        StubJobKind::Midi(_) => "Extracting MIDI",
                        _ => "Splitting stems",
                    };
                    out.push(AiEvent::Progress { job: job.id, label: label.into(), fraction: f });
                }
                continue;
            }
            finished.push(job.id);
            match &job.kind {
                StubJobKind::Midi(name) => {
                    // A plausible A-minor vocal line.
                    let line = [(69, 0, 2), (72, 2, 2), (71, 4, 1), (69, 5, 3), (67, 8, 2), (64, 10, 2), (65, 12, 2), (64, 14, 2)];
                    let notes = line
                        .iter()
                        .map(|&(key, s, l)| DetectedNote {
                            key,
                            start: s * PPQ / 2,
                            length: l * PPQ / 2,
                            velocity: 0.78,
                        })
                        .collect();
                    out.push(AiEvent::MidiReady { job: job.id, source_name: format!("{name} (MIDI)"), notes });
                }
                StubJobKind::Stems(path) => {
                    let stem = |s: &str| (s.to_owned(), path.with_extension(format!("{}.wav", s.to_lowercase())));
                    out.push(AiEvent::StemsReady {
                        job: job.id,
                        stems: vec![stem("Vocals"), stem("Drums"), stem("Bass"), stem("Other")],
                    });
                }
                StubJobKind::Chat(text, proposal) => {
                    out.push(AiEvent::Reply {
                        text: format!(
                            "Stub assistant: I'd answer \"{}\" once a real agent is connected over MCP. Mention a track name and I'll propose an edit.",
                            text.chars().take(80).collect::<String>()
                        ),
                    });
                    if let Some((label, commands)) = proposal.clone() {
                        out.push(AiEvent::Proposal { label, agent: "Stub assistant".into(), commands });
                    }
                }
                StubJobKind::Fail(message) => out.push(AiEvent::Failed { job: job.id, message: message.clone() }),
                StubJobKind::Analyze(path) => out.push(AiEvent::Analyzed { job: job.id, path: path.clone(), analysis: AudioAnalysis::default() }),
            }
        }
        self.jobs.retain(|j| !finished.contains(&j.id));
        out
    }

    fn busy(&self) -> bool {
        !self.jobs.is_empty()
    }
}

/// All bridges the UI uses, boxed so real implementations can replace the stubs.
pub struct Bridges {
    pub engine: Box<dyn EngineBridge>,
    pub plugins: Box<dyn PluginCatalog>,
    pub library: Box<dyn LibraryBridge>,
    pub ai: Box<dyn AiBridge>,
}

impl Bridges {
    pub fn stubs() -> Self {
        Self {
            engine: Box::<StubEngine>::default(),
            plugins: Box::<StubCatalog>::default(),
            library: Box::<StubLibrary>::default(),
            ai: Box::<StubAi>::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_engine_plays_and_stops() {
        let mut e = StubEngine::default();
        let p = Project::new("t");
        e.update_project(&p);
        assert!(!e.transport().playing);
        e.play();
        assert!(e.transport().playing);
        e.stop();
        assert!(!e.transport().playing);
        e.stop();
        assert_eq!(e.transport().position, 0);
    }

    #[test]
    fn export_rules() {
        let mut o = ExportOptions {
            format: ExportFormat::Mp3,
            sample_rate: 48_000,
            range: ExportRange::Song,
            mode: ExportMode::Stems { tracks: vec![TrackId(1)] },
            normalize: true,
            tail_seconds: 2.,
            folder: PathBuf::from("out"),
            file_name: "Song".into(),
        };
        assert!(o.validate().is_err(), "stems cannot be MP3");
        o.format = ExportFormat::Wav(WavDepth::Int24);
        assert!(o.validate().is_ok());
        o.mode = ExportMode::Stems { tracks: vec![] };
        assert!(o.validate().is_err());
        o.mode = ExportMode::Mixdown;
        o.format = ExportFormat::Mp3;
        assert!(o.validate().is_ok());
    }

    #[test]
    fn audio_config_clamps_to_the_device() {
        let mut e = StubEngine::default();
        let dev = e.audio_devices().into_iter().find(|d| d.output_channels > 0).unwrap();
        let cfg = AudioConfig { host: dev.host, output: Some(dev.id.clone()), sample_rate: 192_000, buffer_size: 1, ..AudioConfig::default() };
        let applied = e.set_audio_config(cfg).unwrap();
        assert!(dev.sample_rates.contains(&applied.sample_rate));
        assert!(applied.buffer_size >= dev.buffer_range.0);
        assert!((AudioConfig { sample_rate: 48_000, buffer_size: 480, ..AudioConfig::default() }.buffer_ms() - 10.).abs() < 1e-3);
    }

    #[test]
    fn library_filters_by_folder_and_key() {
        let lib = StubLibrary::default();
        assert!(!lib.items("categories/drums/kicks").is_empty());
        assert!(lib.items("categories/drums").len() >= lib.items("categories/drums/kicks").len());
        let am = Scale::new(Key(9), ScaleKind::Minor);
        let found = lib.search("", Some(am));
        assert!(found.iter().all(|i| i.key.is_none_or(|k| k.mask() == am.mask())));
        assert!(lib.search("kick", None).iter().all(|i| i.name.to_lowercase().contains("kick") || i.category == "Kick"));
    }

    #[test]
    fn catalog_has_params_for_effects() {
        let c = StubCatalog::default();
        assert!(c.device("fx.eq").is_some_and(|d| !d.params.is_empty()));
        assert!(!c.external_plugins().is_empty());
    }
}
