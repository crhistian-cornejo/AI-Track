//! The boundary between the interface and the rest of AI-Track.
//!
//! The UI talks to the audio engine, the plugin catalogue, the sample
//! library and the AI features only through these traits. Each has a stub
//! that returns demo data (and fakes a playing transport and moving meters),
//! so every view can be exercised without the real crates. The adapters over
//! the real crates live in `crate::real`; `AITRACK_STUBS=1` runs the app on
//! the stubs.

use aitrack_dsp::ParamInfo;
use aitrack_model::{DeviceId, DeviceKind, Key, PluginFormat, Project, Scale, ScaleKind, SceneId, Tick, TrackId, TrackKind, PPQ};
use std::collections::{BTreeMap, HashMap};
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

    // ── Analysis (meter strip, device editors) ──
    /// Turn the analysis tap on or off. Off, the audio thread skips it; it
    /// also pauses by itself when nothing drains it (window hidden).
    fn set_analysis(&mut self, _on: bool) {}
    /// Tap the Main output (`None`) or one track's post-fader output.
    fn set_analysis_source(&mut self, _track: Option<TrackId>) {}
    /// Append the stereo frames produced since the last call (oldest first)
    /// and return their sample rate. Call once per painted frame.
    fn drain_analysis(&mut self, _out: &mut Vec<[f32; 2]>) -> u32 {
        48_000
    }
    /// A stock device's display data (`Processor::meter()`: spectrum, gain
    /// reduction… in the layout its plugin documents). Returns how many
    /// values were written; 0 when the device has none.
    fn device_meter(&mut self, _device: DeviceId, _out: &mut [f32]) -> usize {
        0
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
    /// Stock plugin id of every device, for the synthetic device meters.
    devices: HashMap<DeviceId, String>,
    analysis: bool,
    analysis_at: Instant,
    /// Frames of synthetic audio produced so far.
    synth_pos: u64,
    noise: u32,
    born: Instant,
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
            devices: HashMap::new(),
            analysis: false,
            analysis_at: Instant::now(),
            synth_pos: 0,
            noise: 0x1234_5678,
            born: Instant::now(),
        }
    }
}

/// Frames of a synthetic four-on-the-floor loop (kick, off-beat bass, a
/// detuned chord, hats) starting at frame `start` of a 48 kHz stream. The
/// stub engine feeds this to the analysers so every meter has something to
/// show without an audio device.
pub fn synth_demo(start: u64, frames: usize, bpm: f64, seed: &mut u32, out: &mut Vec<[f32; 2]>) {
    use std::f64::consts::TAU;
    let sr = 48_000.0f64;
    let beat_len = 60.0 / bpm.max(20.0);
    let noise = |s: &mut u32| {
        *s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (*s >> 8) as f32 / 8_388_608.0 - 1.0
    };
    let (mut prev_l, mut prev_r) = (0.0f32, 0.0f32);
    for i in 0..frames as u64 {
        let t = (start + i) as f64 / sr;
        let beat = t / beat_len;
        let in_beat = beat.fract() * beat_len;
        // Kick: a sine falling from 120 Hz to 45 Hz.
        let kick = ((TAU * (45.0 * in_beat + 2.5 * (1.0 - (-30.0 * in_beat).exp()))).sin() * (-14.0 * in_beat).exp() * 0.55) as f32;
        // Bass on the off-beat.
        let off = (beat.fract() - 0.5).max(0.0) * beat_len;
        let gate = if beat.fract() >= 0.5 { (1.0 - (-off * 200.0).exp()) * (-off * 5.0).exp() } else { 0.0 };
        let w = TAU * 55.0 * t;
        let bass = ((w.sin() + 0.5 * (2.0 * w).sin() + 0.33 * (3.0 * w).sin()) * gate * 0.16) as f32;
        // A minor chord, detuned apart between the channels.
        let swell = 0.6 + 0.4 * (TAU * t / (8.0 * beat_len)).sin();
        let (mut pad_l, mut pad_r) = (0.0f64, 0.0f64);
        for f in [220.0, 261.63, 329.63, 440.0] {
            pad_l += (TAU * f * 1.003 * t).sin();
            pad_r += (TAU * f * 0.997 * t + 0.6).sin();
        }
        // Hats on the eighths, mostly uncorrelated.
        let eighth = (beat * 2.0).fract() * beat_len * 0.5;
        let hat_env = (-eighth * 70.0).exp() as f32 * if (beat * 2.0) as u64 % 2 == 1 { 0.16 } else { 0.07 };
        let (nl, nr) = (noise(seed), noise(seed));
        let (hl, hr) = (nl - prev_l, nr - prev_r);
        prev_l = nl;
        prev_r = nr;
        let l = kick + bass + (pad_l * swell * 0.035) as f32 + hl * hat_env;
        let r = kick + bass + (pad_r * swell * 0.035) as f32 + hr * hat_env;
        out.push([l * 0.8, r * 0.8]);
    }
}

/// Plausible `meter()` values for a stock plugin, in its documented layout.
fn stub_device_meter(plugin: &str, playing: bool, beat: f32, secs: f32, out: &mut [f32]) -> usize {
    let mut put = |values: &[f32]| {
        let n = values.len().min(out.len());
        out[..n].copy_from_slice(&values[..n]);
        n
    };
    // A kick-shaped envelope per beat and a hat-shaped one per eighth.
    let env = if playing { (-beat.fract() * 5.0).exp() } else { 0.0 };
    let hat = if playing { (-(beat * 2.0).fract() * 6.0).exp() } else { 0.0 };
    let level = if playing { -24.0 + 18.0 * env } else { -120.0 };
    let spectrum = |i: usize| -> f32 {
        if !playing {
            return -120.0;
        }
        let f = 20.0 * 1000f32.powf(i as f32 / 127.0);
        let oct = (f / 60.0).log2();
        let body = -28.0 - 3.2 * oct.abs() + 3.0 * (secs * 2.3 + i as f32 * 0.37).sin();
        let kick = 20.0 * env * (-(f / 55.0).log2().powi(2) * 1.5).exp();
        let air = 12.0 * hat * ((f - 5000.0) / 6000.0).clamp(0.0, 1.0);
        (body + kick + air).max(-110.0)
    };
    match plugin {
        "fx.eq" => {
            let mut v = [0.0f32; 265];
            for (i, s) in v[128..256].iter_mut().enumerate() {
                *s = spectrum(i);
            }
            for (b, d) in v[256..264].iter_mut().enumerate() {
                *d = -3.0 * env * if b % 3 == 1 { 1.0 } else { 0.4 };
            }
            put(&v)
        }
        "fx.analyzer" => {
            let mut v = [0.0f32; 139];
            for (i, s) in v[..128].iter_mut().enumerate() {
                *s = spectrum(i);
            }
            let lufs = if playing { -13.5 + 2.0 * env } else { -120.0 };
            v[128..139].copy_from_slice(&[lufs, lufs - 0.8, lufs - 1.2, 6.5, -1.2, -1.4, 0.7 * env + 0.1, 0.66 * env + 0.1, 0.25, 0.24, 0.62]);
            put(&v)
        }
        "fx.compressor" => put(&[-7.0 * env, level, level - 7.0 * env + 3.0]),
        "fx.gate" => put(&[if env > 0.25 { 0.0 } else { -40.0 * (1.0 - env * 4.0) }, level, if env > 0.25 { 1.0 } else { 0.0 }]),
        "fx.transient" => put(&[if playing { 6.0 * env - 2.0 * (1.0 - env) * hat } else { 0.0 }]),
        "fx.limiter" => put(&[-4.0 * env, if playing { -1.0 } else { -120.0 }, if playing { -5.0 + 6.0 * env } else { -120.0 }]),
        "fx.saturator" => put(&[0.7 * env, 0.6 * env, 0.6 * env, 0.3 * hat * env.max(0.2), 0.25 * hat]),
        "fx.reverb" => put(&[0.3 * env + 0.1 * hat, 0.28 * env + 0.12 * hat, 2.4]),
        "fx.delay" => put(&[0.3 * hat, 0.26 * (1.0 - hat) * env, 1.0 - 0.3 * env]),
        "fx.chorus" => put(&[(secs * 0.8).fract()]),
        "fx.utility" => put(&[0.7 * env, 0.64 * env, 0.55 + 0.3 * (secs * 0.7).sin()]),
        "fx.filter" => put(&[0.0, 0.0, env]),
        "instrument.wavetable" => put(&[if playing { 3.0 } else { 0.0 }, 0.5 * env, 0.46 * env]),
        "instrument.sampler" => put(&[if playing { 1.0 } else { 0.0 }, if playing { beat.fract() } else { 0.0 }, 0.5 * env]),
        "instrument.drum" => {
            let mut v = [0.0f32; 18];
            v[0] = if playing { 2.0 } else { 0.0 };
            v[1] = if playing { ((beat * 2.0) as usize % 4) as f32 } else { -1.0 };
            for (i, l) in v[2..].iter_mut().enumerate() {
                *l = if i % 4 == 0 { env } else if i % 4 == 2 { hat * 0.7 } else { 0.0 };
            }
            put(&v)
        }
        _ => 0,
    }
}

fn collect_devices(devices: &[aitrack_model::Device], out: &mut HashMap<DeviceId, String>) {
    for d in devices {
        match &d.kind {
            DeviceKind::Stock { plugin_id } => {
                out.insert(d.id, plugin_id.clone());
            }
            DeviceKind::Rack(r) => {
                for c in &r.chains {
                    collect_devices(&c.devices, out);
                }
            }
            DeviceKind::Plugin { .. } => {}
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
        self.devices.clear();
        for t in &project.tracks {
            collect_devices(&t.devices, &mut self.devices);
        }
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

    fn set_analysis(&mut self, on: bool) {
        if on && !self.analysis {
            self.analysis_at = Instant::now();
        }
        self.analysis = on;
    }

    fn drain_analysis(&mut self, out: &mut Vec<[f32; 2]>) -> u32 {
        const SR: u32 = 48_000;
        let now = Instant::now();
        // A stalled UI resumes from now, as an overrun ring would.
        let dt = now.duration_since(self.analysis_at).as_secs_f64().min(0.25);
        self.analysis_at = now;
        if !self.analysis {
            return SR;
        }
        let frames = (dt * SR as f64).round() as usize;
        if self.state.playing {
            synth_demo(self.synth_pos, frames, self.bpm, &mut self.noise, out);
        } else {
            out.extend(std::iter::repeat_n([0.0; 2], frames));
        }
        self.synth_pos += frames as u64;
        SR
    }

    fn device_meter(&mut self, device: DeviceId, out: &mut [f32]) -> usize {
        let Some(plugin) = self.devices.get(&device) else { return 0 };
        let beat = self.now_tick() as f32 / PPQ as f32;
        stub_device_meter(plugin, self.state.playing, beat, self.born.elapsed().as_secs_f32(), out)
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

impl Default for StubCatalog {
    fn default() -> Self {
        // The real stock devices: listing them needs no audio device, and the
        // demo project (and every device editor) uses their parameter ids.
        let stock = crate::real::plugins::stock_devices();
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
    /// Listen to every sample and detect its key, tempo and type (in the
    /// background, one file at a time). File names alone rarely carry a key,
    /// so "Matches project" needs this to have much to show.
    fn detect_keys(&mut self) -> Result<(), String> {
        Err("Key detection needs the real library".into())
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
    fn stub_engine_feeds_the_analysers() {
        let mut e = StubEngine::default();
        let p = crate::demo::project();
        e.update_project(&p);
        let mut frames = Vec::new();
        assert_eq!(e.drain_analysis(&mut frames), 48_000);
        assert!(frames.is_empty(), "nothing while analysis is off");
        e.set_analysis(true);
        e.play();
        std::thread::sleep(std::time::Duration::from_millis(30));
        e.drain_analysis(&mut frames);
        assert!(frames.len() > 400 && frames.len() < 48_000 / 3);
        assert!(frames.iter().any(|f| f[0].abs() > 0.01));
        assert!(frames.iter().all(|f| f[0].abs() <= 1.0 && f[1].abs() <= 1.0));
        // Device meters follow each plugin's documented layout.
        let eq = p.tracks.iter().flat_map(|t| &t.devices).find(|d| matches!(&d.kind, DeviceKind::Stock { plugin_id } if plugin_id == "fx.eq")).unwrap().id;
        let mut m = [0.0f32; 320];
        assert_eq!(e.device_meter(eq, &mut m), 265);
        assert!(m[128..256].iter().any(|v| *v > -60.0));
        assert_eq!(e.device_meter(DeviceId(u64::MAX), &mut m), 0);
        // The demo loop is deterministic and continuous across calls.
        let (mut a, mut b, mut seed) = (Vec::new(), Vec::new(), 7u32);
        synth_demo(0, 512, 128.0, &mut seed, &mut a);
        synth_demo(512, 512, 128.0, &mut seed, &mut a);
        let mut seed = 7u32;
        synth_demo(0, 1024, 128.0, &mut seed, &mut b);
        assert_eq!(a.len(), 1024);
        assert!(a.iter().zip(&b).all(|(x, y)| (x[0] - y[0]).abs() < 0.2), "kick, bass and chord line up");
    }

    #[test]
    fn catalog_has_params_for_effects() {
        let c = StubCatalog::default();
        assert!(c.device("fx.eq").is_some_and(|d| !d.params.is_empty()));
        assert!(!c.external_plugins().is_empty());
    }
}
