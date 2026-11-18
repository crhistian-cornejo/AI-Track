//! The boundary between the interface and the rest of AI-Track.
//!
//! The UI talks to the audio engine, the plugin catalogue, the sample
//! library and the AI features only through these traits. Each has a stub
//! that returns demo data (and fakes a playing transport and moving meters),
//! so every view can be exercised without the real crates. The adapters over
//! the real crates live in `crate::real`; `AITRACK_STUBS=1` runs the app on
//! the stubs.

use aitrack_dsp::ParamInfo;
use aitrack_model::{DeviceId, DeviceKind, InputSource, Key, Monitor, PluginFormat, Project, Quantize, Scale, ScaleKind, SceneId, Tick, TrackId, TrackKind, PPQ};
use std::collections::{BTreeMap, BTreeSet, HashMap};
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

/// A parameter edit made in a hosted plugin's own window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PluginEdit {
    pub device: DeviceId,
    pub param: u32,
    /// Normalised 0..=1; meaningful for [`PluginEditKind::Value`].
    pub value: f32,
    pub kind: PluginEditKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginEditKind {
    /// The user grabbed a control.
    Begin,
    Value,
    /// The user let go: close the undo step.
    End,
}

pub trait EngineBridge {
    fn play(&mut self);
    fn stop(&mut self);
    fn toggle_record(&mut self);
    fn set_looping(&mut self, on: bool);
    fn set_metronome(&mut self, on: bool);
    /// Bars of metronome count-in before playback starts (0 = off).
    fn set_count_in(&mut self, _bars: u32) {}
    fn count_in(&self) -> u32 {
        0
    }
    /// Quantisation of scene launches into empty slots and track stops.
    fn set_global_quantize(&mut self, _quantize: Quantize) {}
    fn global_quantize(&self) -> Quantize {
        Quantize::Bar
    }
    /// Stop the Session clips and let every track follow the arrangement again.
    fn back_to_arrangement(&mut self) {}
    fn locate(&mut self, tick: Tick);
    fn transport(&self) -> TransportState;
    /// A new snapshot of the project after an edit (the engine compiles it off the audio thread).
    fn update_project(&mut self, project: &Project);
    /// Where recording stands (a take being captured, and on which tracks).
    fn recording_state(&self) -> RecordingState {
        RecordingState::default()
    }
    /// The WAV files that finished since the last call (one per armed audio
    /// track of a take). They are written a moment after the take ends: ask
    /// until the ones expected are in.
    fn finish_audio_recording(&mut self) -> Vec<AudioTake> {
        Vec::new()
    }
    /// Where the engine writes the audio takes.
    fn set_recording_dir(&mut self, _dir: &Path) {}
    /// The hardware channels an audio track records and monitors.
    fn set_track_input(&mut self, _track: TrackId, _input: InputSource) {}
    /// How an audio track is monitored while armed.
    fn set_track_monitor(&mut self, _track: TrackId, _monitor: Monitor) {}
    /// The notes of the take that are complete so far, each handed out once.
    /// Call it now and then during a take.
    fn take_recorded_notes(&mut self) -> Vec<RecNote> {
        Vec::new()
    }
    /// The notes played on armed tracks while the transport rolled without
    /// recording, each handed out once (Capture MIDI).
    fn take_captured_notes(&mut self) -> Vec<RecNote> {
        Vec::new()
    }
    /// The finished take (the notes not handed out yet), once, after it ended.
    fn finish_midi_recording(&mut self) -> Option<MidiTake> {
        None
    }
    /// The MIDI input ports of the system, as of the last look (every 3 s).
    fn midi_ports(&self) -> Vec<MidiPortState> {
        Vec::new()
    }
    /// Switch a port on or off (remembered; opening happens in the background).
    fn set_midi_port_enabled(&mut self, _id: &str, _enabled: bool) {}
    /// The track live MIDI goes to; `None` = every armed instrument track.
    fn midi_target(&self) -> Option<TrackId> {
        None
    }
    /// Choose that track (remembered).
    fn set_midi_target(&mut self, _track: Option<TrackId>) {}
    /// MIDI messages read since the engine started: an activity light.
    fn midi_message_count(&self) -> u64 {
        0
    }
    /// Bytes of the sample cache held by warp renders (part of its total).
    fn sample_cache_warp_bytes(&self) -> usize {
        0
    }
    /// Fast path for a knob in a drag: set a device parameter (normalised) on
    /// the running graph at once, no compile and no project snapshot. `false`
    /// when the engine cannot take it (the caller sends the project instead).
    fn fast_param(&mut self, _device: DeviceId, _param: u32, _normalized: f32) -> bool {
        false
    }
    /// Fast path for a fader: a track's volume in dB.
    fn fast_track_volume(&mut self, _track: TrackId, _db: f32) -> bool {
        false
    }
    fn fast_track_pan(&mut self, _track: TrackId, _pan: f32) -> bool {
        false
    }
    /// Fast path for a send knob: the level in dB of send `index` of the track.
    fn fast_send_level(&mut self, _track: TrackId, _index: usize, _db: f32) -> bool {
        false
    }
    /// Fast path while a rack's chain selector is dragged: the zone gains
    /// follow within the block, no compile.
    fn fast_rack_selector(&mut self, _rack: DeviceId, _value: u8) -> bool {
        false
    }
    /// Graphs the engine has compiled and project snapshots it absorbed
    /// without compiling (tests of the fast path; 0 for engines that don't count).
    #[allow(dead_code)]
    fn compile_count(&self) -> u64 {
        0
    }
    #[allow(dead_code)]
    fn patch_count(&self) -> u64 {
        0
    }
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
    /// Measured latency of the running audio device, for Preferences → Audio.
    fn audio_latency(&self) -> Option<AudioLatency> {
        None
    }

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
    /// What the engine reports about itself (read again only when it changes).
    fn health(&self) -> EngineHealth {
        EngineHealth::default()
    }
    /// A non-blocking problem to show above the workspace (no audio device…).
    fn banner(&self) -> Option<String> {
        None
    }
    fn dismiss_banner(&mut self) {}
    /// Persistent warning when a sandboxed plugin hung/crashed or failed to load.
    fn watchdog_warning(&self) -> Option<String> {
        None
    }
    fn dismiss_watchdog_warning(&mut self) {}
    /// Warning text for a specific device whose host hung, crashed, or failed to start.
    fn device_warning(&self, _device: aitrack_model::DeviceId) -> Option<String> {
        None
    }
    /// Processor factory for offline clip consolidation and audio rendering.
    fn export_factory(&self) -> std::sync::Arc<dyn aitrack_engine::ProcessorFactory> {
        std::sync::Arc::new(aitrack_engine::NullFactory)
    }
    /// Memory budget for decoded audio files in MB.
    fn set_sample_cache_mb(&mut self, _mb: usize) {}
    /// `(used_bytes, budget_bytes)` of decoded audio in memory.
    fn sample_cache_stats(&self) -> (usize, usize) {
        (0, 512 * 1024 * 1024)
    }
    /// Short status-bar text ("WASAPI · 48 kHz · 256").
    fn status_label(&self) -> String {
        "Engine: stub".into()
    }

    // ── Hosted plugins ──
    /// Open a VST3/CLAP device's editor window (main thread).
    fn open_editor(&mut self, _device: aitrack_model::DeviceId) -> Result<(), String> {
        Err("Plugin windows need the real engine".into())
    }
    /// Main thread: the current state of every loaded VST3/CLAP device, so
    /// the UI can store it in the project (`SetDeviceState`) before saving.
    /// Changes made in a plugin's own window live only in the plugin until then.
    fn plugin_states(&mut self) -> Vec<(DeviceId, Vec<u8>)> {
        Vec::new()
    }
    /// Parameter edits made in plugin windows since the last call, oldest
    /// first, for the UI to turn into `SetParam` commands (one undo step per
    /// `Begin`…`End` gesture).
    fn take_plugin_edits(&mut self) -> Vec<PluginEdit> {
        Vec::new()
    }
    /// Run third-party plugins out of process (applies to newly loaded instances).
    fn sandbox_plugins(&self) -> bool {
        false
    }
    fn set_sandbox_plugins(&mut self, _on: bool) {}
    /// The "Audio threads" preference: `0` = Auto, otherwise how many threads
    /// (the audio thread included) the graph and offline renders run on.
    fn audio_threads(&self) -> usize {
        0
    }
    fn set_audio_threads(&mut self, _setting: usize) {}
    /// Threads the running graph actually uses.
    fn active_audio_threads(&self) -> usize {
        1
    }
    /// Audition an audio file at a low level (`None` stops).
    fn preview(&mut self, _path: Option<&Path>) {}
    /// Audition a sound adjusted to the song: stretched to its tempo and
    /// transposed to its key, entering on the next bar when the song runs.
    fn preview_synced(&mut self, _req: &SyncedPreview) {}
    /// Level of the preview, 0..=1.
    fn set_preview_volume(&mut self, _volume: f32) {}
    /// Audition an audio file the way a clip plays it: `ratio` times faster
    /// (transpose and detune) and backwards when `reverse`.
    fn preview_pitched(&mut self, path: Option<&Path>, _ratio: f64, _reverse: bool) {
        self.preview(path);
    }
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
    count_in: u32,
    global_quantize: Quantize,
    audio: AudioConfig,
    /// Stock plugin id of every device, for the synthetic device meters.
    devices: HashMap<DeviceId, String>,
    analysis: bool,
    analysis_at: Instant,
    /// Frames of synthetic audio produced so far.
    synth_pos: u64,
    noise: u32,
    born: Instant,
    /// What the "plugins" report as their live state (tests of save/export).
    pub live_states: Vec<(DeviceId, Vec<u8>)>,
    /// Project snapshots received, and fast-path calls (tests of drags).
    pub project_updates: u64,
    pub fast_calls: u64,
    /// Stub MIDI: the ports switched off and the chosen target.
    midi_off: BTreeSet<String>,
    midi_to: Option<TrackId>,
    /// Stub recording: the armed tracks, the notes a take has played so far and
    /// the take that ends when the transport stops (tests set these).
    pub armed_midi: Vec<TrackId>,
    pub armed_audio: Vec<TrackId>,
    /// Files the take wrote (handed out once) and what the engine was told.
    pub audio_takes: Vec<AudioTake>,
    pub track_io: BTreeMap<TrackId, (InputSource, Monitor)>,
    pub rec_dir: Option<PathBuf>,
    pub played: Vec<RecNote>,
    pub rec_take: Option<MidiTake>,
    rec_started: Option<Tick>,
    /// The last synced preview asked for, and the preview level (tests).
    pub last_synced: Option<SyncedPreview>,
    pub preview_volume: f32,
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
            count_in: 0,
            global_quantize: Quantize::Bar,
            audio: AudioConfig::default(),
            devices: HashMap::new(),
            analysis: false,
            analysis_at: Instant::now(),
            synth_pos: 0,
            noise: 0x1234_5678,
            born: Instant::now(),
            live_states: Vec::new(),
            project_updates: 0,
            fast_calls: 0,
            midi_off: BTreeSet::new(),
            midi_to: None,
            armed_midi: Vec::new(),
            armed_audio: Vec::new(),
            audio_takes: Vec::new(),
            track_io: BTreeMap::new(),
            rec_dir: None,
            played: Vec::new(),
            rec_take: None,
            rec_started: None,
            last_synced: None,
            preview_volume: 1.,
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
        "fx.drum_extract" => {
            let mut v = [0.0f32; 70];
            for (i, s) in v[..64].iter_mut().enumerate() {
                *s = spectrum(i * 2);
            }
            v[64..70].copy_from_slice(&[env, 0.6 * env, hat, (env * 0.7 + hat * 0.3).min(1.0), 0.7 * env + 0.08, 0.68 * env + 0.08]);
            put(&v)
        }
        "fx.compressor" => {
            // Header, the 64-point static curve (the editor draws its own), latency, style, feedback.
            let mut v = [0.0f32; 70];
            v[..3].copy_from_slice(&[-7.0 * env, level, level - 7.0 * env + 3.0]);
            put(&v)
        }
        "fx.buscomp" => {
            let mut v = [0.0f32; 70];
            v[..3].copy_from_slice(&[-5.0 * env, level, level - 5.0 * env + 2.0]);
            v[68] = if playing && env > 0.8 { 1.0 } else { 0.0 };
            v[69] = 200.0 + 400.0 * (1.0 - env);
            put(&v)
        }
        "fx.clipper" => put(&[if playing { -6.0 * env } else { 0.0 }, if playing { -9.0 + 12.0 * env } else { -120.0 }, if playing { -3.0 - 1.5 * (1.0 - env) } else { -120.0 }]),
        "fx.mbdyn" => {
            // Band levels and applied gains, band peaks, then in/out peaks, latency and flags; the curves stay zero.
            let mut v = vec![0.0f32; aitrack_stock::fx::mbdyn::METER_LEN];
            let lv = |k: f32| if playing { -26.0 + k * 14.0 * env - 6.0 * (1.0 - env) } else { -120.0 };
            let bands = [lv(1.3), lv(0.8) + 4.0 * hat, lv(0.3) + 8.0 * hat];
            for (i, l) in bands.iter().enumerate() {
                v[aitrack_stock::fx::mbdyn::METER_LEVEL + i] = *l;
                v[aitrack_stock::fx::mbdyn::METER_GAIN + i] = if playing { [-5.0 * env, -2.0 * env + 1.5 * (1.0 - env), 2.5 * (1.0 - hat) - 1.0 * hat][i] } else { 0.0 };
                v[aitrack_stock::fx::mbdyn::METER_BAND_PEAK + i] = if playing { *l + 4.0 } else { -120.0 };
            }
            v[aitrack_stock::fx::mbdyn::METER_IN_PEAK] = if playing { -4.0 + 3.0 * env } else { -120.0 };
            v[aitrack_stock::fx::mbdyn::METER_OUT_PEAK] = if playing { -5.0 + 2.0 * env } else { -120.0 };
            put(&v)
        }
        "fx.drumbus" => {
            let mut v = [0.0f32; 135];
            v[..7].copy_from_slice(&[-4.0 * env, level + 2.0, level - 1.0, 0.0, 0.4 * env, (beat.floor()).max(0.0), 3.0 * env - 1.0]);
            for i in 0..64 {
                let t = i as f32 / 63.0;
                // Key (low band of the input) and the synthesised boom, as a decaying hit every beat.
                let hit = if playing { (-(t * 4.0 + beat.fract()).rem_euclid(1.0) * 6.0).exp() } else { 0.0 };
                v[71 + i] = 0.5 * hit;
                v[7 + i] = 0.35 * hit * (0.6 + 0.4 * (i as f32 * 0.5).sin().abs());
            }
            put(&v)
        }
        "fx.gate" => put(&[if env > 0.25 { 0.0 } else { -40.0 * (1.0 - env * 4.0) }, level, if env > 0.25 { 1.0 } else { 0.0 }]),
        "fx.transient" => put(&[if playing { 6.0 * env - 2.0 * (1.0 - env) * hat } else { 0.0 }]),
        "fx.limiter" => put(&[
            -4.0 * env,
            if playing { -1.0 } else { -120.0 },
            if playing { -5.0 + 6.0 * env } else { -120.0 },
            if playing { -11.8 } else { -120.0 },
            if playing { -9.6 + 1.5 * env } else { -120.0 },
            if playing { 6.4 } else { 0.0 },
            if playing { -8.0 + 3.0 * env } else { -120.0 },
        ]),
        "fx.saturator" => put(&[0.7 * env, 0.6 * env, 0.6 * env, 0.3 * hat * env.max(0.2), 0.25 * hat]),
        // Tape, amp and console publish their peaks (linear) and one activity value.
        "fx.tape" | "fx.amp" => put(&[0.7 * env, 0.6 * env, 0.5 * env + 0.1 * hat]),
        "fx.console" => put(&[0.7 * env, 0.6 * env, 4.0 * env]),
        "fx.shaper" => put(&[beat.fract(), if playing { 0.15 + 0.85 * beat.fract().min(0.6) / 0.6 } else { 1.0 }]),
        "fx.reverb" => put(&[0.3 * env + 0.1 * hat, 0.28 * env + 0.12 * hat, 2.4]),
        "fx.delay" => put(&[0.3 * hat, 0.26 * (1.0 - hat) * env, 1.0 - 0.3 * env]),
        "fx.chorus" => put(&[(secs * 0.8).fract()]),
        "fx.pitchshift" => {
            let ph = (secs * 1.6).fract();
            let win = |x: f32| (std::f32::consts::PI * x.rem_euclid(1.0)).sin().powi(2);
            put(&[7.0, ph, (ph + 0.5).fract(), win(ph), win(ph + 0.5), 40.0, 0.5 * env, 0.45 * env])
        }
        "fx.freqshift" => put(&[120.0 + 80.0 * (secs * 0.7).sin(), -(120.0 + 80.0 * (secs * 0.7).sin()), 0.5 * env, 0.45 * env, (secs * 0.7).sin()]),
        "fx.vocoder" => {
            let mut v = [0.0f32; 20];
            v[0] = 16.0;
            v[1] = 140.0;
            v[2] = 1.0;
            v[3] = 0.6 * hat;
            for k in 0..16 {
                let t = k as f32 / 15.0;
                let formants = (-(t - 0.25 - 0.1 * (secs * 0.9).sin()).powi(2) * 40.0).exp() + 0.7 * (-(t - 0.6).powi(2) * 60.0).exp();
                v[4 + k] = if playing { (0.15 + 0.7 * formants * env).clamp(0.0, 1.0) } else { 0.0 };
            }
            put(&v)
        }
        "fx.utility" => put(&[0.7 * env, 0.64 * env, 0.55 + 0.3 * (secs * 0.7).sin()]),
        "fx.filter" => put(&[0.0, 0.0, env]),
        "instrument.wavetable" => put(&[if playing { 3.0 } else { 0.0 }, 0.5 * env, 0.46 * env]),
        "instrument.acid303" => put(&[if playing { 1.0 } else { 0.0 }, if playing { ((beat * 4.0) as usize % 16) as f32 } else { -1.0 }, 0.6 * env, env]),
        "instrument.kick808" => put(&[if playing { 1.0 } else { 0.0 }, 0.25, 0.85 * env, env]),
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

    fn set_count_in(&mut self, bars: u32) {
        self.count_in = bars;
    }

    fn count_in(&self) -> u32 {
        self.count_in
    }

    fn set_global_quantize(&mut self, quantize: Quantize) {
        self.global_quantize = quantize;
    }

    fn global_quantize(&self) -> Quantize {
        self.global_quantize
    }

    fn back_to_arrangement(&mut self) {
        self.slots.clear();
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

    fn preview_synced(&mut self, req: &SyncedPreview) {
        self.last_synced = Some(req.clone());
    }

    fn set_preview_volume(&mut self, volume: f32) {
        self.preview_volume = volume.clamp(0., 1.);
    }

    fn recording_state(&self) -> RecordingState {
        let capturing = self.state.recording && self.state.playing;
        RecordingState {
            capturing,
            start_tick: self.rec_started.unwrap_or(0),
            pass: 0,
            tracks: self.armed_midi.clone(),
            audio_tracks: self.armed_audio.clone(),
            takes_started: 0,
            takes_finished: 0,
            dropped: 0,
        }
    }

    fn take_recorded_notes(&mut self) -> Vec<RecNote> {
        std::mem::take(&mut self.played)
    }

    fn finish_audio_recording(&mut self) -> Vec<AudioTake> {
        if self.state.recording && self.state.playing {
            return Vec::new();
        }
        std::mem::take(&mut self.audio_takes)
    }

    fn set_recording_dir(&mut self, dir: &Path) {
        self.rec_dir = Some(dir.to_path_buf());
    }

    fn set_track_input(&mut self, track: TrackId, input: InputSource) {
        self.track_io.entry(track).or_insert((InputSource::None, Monitor::Auto)).0 = input;
    }

    fn set_track_monitor(&mut self, track: TrackId, monitor: Monitor) {
        self.track_io.entry(track).or_insert((InputSource::None, Monitor::Auto)).1 = monitor;
    }

    fn finish_midi_recording(&mut self) -> Option<MidiTake> {
        if self.state.recording && self.state.playing {
            return None;
        }
        self.rec_take.take()
    }

    fn midi_ports(&self) -> Vec<MidiPortState> {
        [("stub-keys", "Stub Keyboard"), ("stub-pads", "Stub Pad Controller")]
            .into_iter()
            .map(|(id, name)| {
                let enabled = !self.midi_off.contains(id);
                MidiPortState { id: id.into(), name: name.into(), enabled, open: enabled }
            })
            .collect()
    }

    fn set_midi_port_enabled(&mut self, id: &str, enabled: bool) {
        if enabled {
            self.midi_off.remove(id);
        } else {
            self.midi_off.insert(id.to_owned());
        }
    }

    fn midi_target(&self) -> Option<TrackId> {
        self.midi_to
    }

    fn set_midi_target(&mut self, track: Option<TrackId>) {
        self.midi_to = track;
    }

    fn fast_param(&mut self, _: DeviceId, _: u32, _: f32) -> bool {
        self.fast_calls += 1;
        true
    }

    fn fast_rack_selector(&mut self, _: DeviceId, _: u8) -> bool {
        self.fast_calls += 1;
        true
    }

    fn fast_track_volume(&mut self, _: TrackId, _: f32) -> bool {
        self.fast_calls += 1;
        true
    }

    fn fast_track_pan(&mut self, _: TrackId, _: f32) -> bool {
        self.fast_calls += 1;
        true
    }

    fn fast_send_level(&mut self, _: TrackId, _: usize, _: f32) -> bool {
        self.fast_calls += 1;
        true
    }

    fn compile_count(&self) -> u64 {
        self.project_updates
    }

    fn update_project(&mut self, project: &Project) {
        self.project_updates += 1;
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

    fn plugin_states(&mut self) -> Vec<(DeviceId, Vec<u8>)> {
        self.live_states.clone()
    }

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

/// Measured audio latency for Preferences → Audio.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AudioLatency {
    pub sample_rate: u32,
    pub buffer_frames: u32,
    pub output_ms: f32,
    pub input_ms: f32,
    pub device_output_ms: f32,
    pub plugin_ms: f32,
}

impl AudioLatency {
    pub fn round_trip_ms(&self) -> f32 {
        self.output_ms + self.input_ms
    }
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

#[derive(Clone, Debug)]
pub struct DeviceInfo {
    /// Stock plugin id, as in `DeviceKind::Stock { plugin_id }` (e.g. `"fx.eq"`).
    pub id: &'static str,
    pub name: &'static str,
    pub tags: &'static [&'static str],
    pub params: Vec<ParamInfo>,
    #[allow(dead_code)]
    pub is_instrument: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ExternalPlugin {
    pub uid: String,
    pub name: String,
    pub vendor: String,
    pub format: PluginFormat,
    pub category: String,
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

/// A folder the plugin scan looks in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginFolder {
    pub path: PathBuf,
    pub format: PluginFormat,
    /// Added by the user; the system's own folders cannot be removed.
    pub custom: bool,
    pub exists: bool,
    /// Plugin files of this format inside it.
    pub files: usize,
}

/// A plugin file that is not in the list, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginProblem {
    pub path: PathBuf,
    pub format: PluginFormat,
    /// "Crashed while loading".
    pub title: String,
    /// What to try.
    pub advice: String,
    /// What the checker said.
    pub detail: String,
}

/// What the last plugin scan found and left out.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PluginReport {
    pub folders: Vec<PluginFolder>,
    pub problems: Vec<PluginProblem>,
    /// Plugin files seen (listed or not).
    pub files: usize,
    /// How long the scan took, once one has finished.
    pub took: Option<std::time::Duration>,
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
    /// The parameters of a loaded VST3/CLAP device, as the plugin reported
    /// them (empty until it is loaded, and for stock devices: see
    /// [`PluginCatalog::device`]). `automatable` is false for read-only and
    /// hidden ones; MIDI-CC proxy parameters are not listed at all.
    fn device_params(&self, _device: DeviceId) -> Vec<ParamInfo> {
        Vec::new()
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
    fn add_plugin_folder(&mut self, _path: PathBuf) -> bool {
        false
    }
    fn remove_plugin_folder(&mut self, _path: &Path) -> bool {
        false
    }
    /// The folders searched and the files left out, as of the last scan.
    fn plugin_report(&self) -> PluginReport {
        PluginReport::default()
    }
    /// Check a file that failed again (it is not skipped this time).
    fn retry_plugin(&mut self, _path: &Path) {}
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
        let ext = |name: &str, vendor: &str, format, category: &str, inst| ExternalPlugin {
            uid: format!("{vendor}.{name}").to_lowercase().replace(' ', "-"),
            name: name.into(),
            vendor: vendor.into(),
            format,
            category: category.into(),
            path: PathBuf::from(match format {
                PluginFormat::Vst3 => format!("C:/Program Files/Common Files/VST3/{name}.vst3"),
                PluginFormat::Clap => format!("C:/Program Files/Common Files/CLAP/{name}.clap"),
            }),
            is_instrument: inst,
        };
        let external = vec![
            ext("Surge XT", "Surge Synth Team", PluginFormat::Clap, "instrument|synthesizer", true),
            ext("Vital", "Vital Audio", PluginFormat::Vst3, "Instrument|Synth", true),
            ext("Dexed", "Digital Suburban", PluginFormat::Vst3, "Instrument|Synth", true),
            ext("OTT", "Xfer Records", PluginFormat::Vst3, "Fx|Dynamics", false),
            ext("Valhalla Supermassive", "Valhalla DSP", PluginFormat::Vst3, "Fx|Reverb", false),
            ext("Airwindows Consolidated", "Airwindows", PluginFormat::Clap, "audio-effect", false),
            ext("Chow Tape Model", "Chowdhury DSP", PluginFormat::Clap, "audio-effect|distortion", false),
            // The same plugins in the other format: the palette shows one row each.
            ext("Surge XT", "Surge Synth Team", PluginFormat::Vst3, "Instrument|Synth", true),
            ext("Vital", "Vital Audio", PluginFormat::Clap, "instrument|synthesizer", true),
            ext("Airwindows Consolidated", "Airwindows", PluginFormat::Vst3, "Fx", false),
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
    fn plugin_report(&self) -> PluginReport {
        // Demo data for the Preferences page: two folders, one missing, two files left out.
        let folder = |path: &str, format, custom, exists, files| PluginFolder { path: PathBuf::from(path), format, custom, exists, files };
        let problem = |file: &str, format, title: &str, advice: &str, detail: &str| PluginProblem {
            path: PathBuf::from(file),
            format,
            title: title.into(),
            advice: advice.into(),
            detail: detail.into(),
        };
        PluginReport {
            folders: vec![
                folder("/Library/Audio/Plug-Ins/VST3", PluginFormat::Vst3, false, true, 12),
                folder("/Library/Audio/Plug-Ins/CLAP", PluginFormat::Clap, false, true, 7),
                folder("/Volumes/Plugins/VST3", PluginFormat::Vst3, true, false, 0),
            ],
            problems: vec![
                problem(
                    "/Library/Audio/Plug-Ins/VST3/Old Reverb.vst3",
                    PluginFormat::Vst3,
                    "Crashed while loading",
                    "The plugin crashed when loaded; AI-Track stayed safe. Update or reinstall it, then retry.",
                    "scanner exited with signal: 11 and no result",
                ),
                problem(
                    "/Library/Audio/Plug-Ins/CLAP/Licensed Synth.clap",
                    PluginFormat::Clap,
                    "Took too long to load",
                    "Often a plugin waiting for a licence or a login window. Open it once in its own app, then retry.",
                    "timed out",
                ),
            ],
            files: 21,
            took: Some(std::time::Duration::from_millis(1090)),
        }
    }
}

// ───────────────────────────── Sounds panel ─────────────────────────────

/// Loops, one-shots or both.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SoundKind {
    #[default]
    All,
    Loops,
    OneShots,
}

/// The broad kinds of sound the panel's chips offer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SoundGroup {
    Drums,
    Bass,
    Synths,
    Instruments,
    Vocals,
    Fx,
}

impl SoundGroup {
    pub const ALL: [SoundGroup; 6] = [Self::Drums, Self::Bass, Self::Synths, Self::Instruments, Self::Vocals, Self::Fx];

    pub fn label(self) -> &'static str {
        match self {
            Self::Drums => "Drums",
            Self::Bass => "Bass",
            Self::Synths => "Synths",
            Self::Instruments => "Instruments",
            Self::Vocals => "Vocals",
            Self::Fx => "FX",
        }
    }
}

/// Which collection a sound comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SoundOrigin {
    /// The sound bank that ships with AI-Track.
    Factory,
    /// The folder the Splice desktop app downloads the user's licensed sounds to.
    Splice,
    /// The user's own folders.
    Mine,
}

impl SoundOrigin {
    pub const ALL: [SoundOrigin; 3] = [Self::Splice, Self::Factory, Self::Mine];

    pub fn label(self) -> &'static str {
        match self {
            Self::Factory => "Factory",
            Self::Splice => "Splice",
            Self::Mine => "Mine",
        }
    }
}

/// A folder of a collection to search inside.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SoundFolder {
    /// Which library: 0 the factory bank, 1 the user's.
    pub library: usize,
    pub root: i64,
    /// Folder inside the root, `/`-separated; `None` = the whole root.
    pub under: Option<String>,
    /// What the chip says.
    pub label: String,
    /// Sounds inside (counted when the list was made).
    pub count: u64,
}

/// What the Sounds panel asks the library for. The key and BPM are the
/// project's when "fit to song" is on; the library only filters by them, the
/// panel works out how each result is adjusted ([`aitrack_library::fit`]).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SoundQuery {
    pub text: String,
    pub kind: SoundKind,
    pub group: Option<SoundGroup>,
    /// Keys that fit this one (relative and Camelot neighbours too), or exactly it.
    pub key: Option<(Scale, bool)>,
    /// Loops whose tempo (½×, 1× or 2×) is within this many percent of the BPM; one-shots always pass.
    pub bpm: Option<(f32, f32)>,
    pub tag: Option<String>,
    pub origin: Option<SoundOrigin>,
    pub favorites: bool,
    pub folder: Option<SoundFolder>,
    /// 0 = a few hundred.
    pub limit: usize,
}

/// One result of the Sounds panel.
#[derive(Clone, Debug, PartialEq)]
pub struct SoundHit {
    pub item: LibraryItem,
    pub origin: SoundOrigin,
    pub tags: Vec<String>,
    pub bpm_trust: BpmTrust,
    /// Missing its key or tempo: the analysis is working on it in the background.
    pub unanalysed: bool,
}

/// Where the Splice folder stands, for the panel's one-line status.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum SpliceState {
    /// Nothing to say (not looked at yet, or a library without folders).
    #[default]
    Unknown,
    /// There is no such folder: the panel says how to choose it.
    Missing(PathBuf),
    /// Indexed as a collection, with this many sounds.
    Indexed { folder: PathBuf, sounds: u64 },
    /// Being read now.
    Scanning(PathBuf),
}

/// What a [`SoundSearchFn`] needs to know that only the UI thread has at hand.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SoundEnv {
    /// The Splice download folder, when there is one.
    pub splice_folder: Option<PathBuf>,
    /// A scan of the library is running.
    pub scanning: bool,
}

/// A sound search to run off the UI thread (see `LibraryBridge::sound_searcher`).
pub type SoundSearchFn = Box<dyn FnMut(&SoundQuery, &SoundEnv) -> SoundPage + Send>;

/// A page of results.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SoundPage {
    pub hits: Vec<SoundHit>,
    /// More matched than `hits` holds.
    pub more: bool,
    /// The genres/tags in the results with their counts, most common first (for the tag chips).
    pub tags: Vec<(String, usize)>,
    /// The folders worth filtering by, most sounds first.
    pub folders: Vec<SoundFolder>,
    pub splice: SpliceState,
}

/// A preview of a sound adjusted to the song (see `EngineBridge::preview_synced`).
#[derive(Clone, Debug, PartialEq)]
pub struct SyncedPreview {
    pub path: PathBuf,
    /// The sound's tempo, read at the ½×/1×/2× that lands nearest the song's; `None` = not stretched.
    pub original_bpm: Option<f64>,
    pub project_bpm: f64,
    /// Transposition in semitones.
    pub semitones: i8,
    /// Pitched material (stretched keeping its pitch) as against percussion.
    pub tonal: bool,
    /// Play again and again (loops); one-shots play once.
    pub looped: bool,
    /// Time until the next bar of the running song, for the sound to enter on it; `None` = at once.
    pub start_in: Option<std::time::Duration>,
    /// Length of a bar at the song's tempo, to keep the entry on the grid if the render takes longer than `start_in`.
    pub bar: std::time::Duration,
}

/// The first Re-Pitch ratio of a plain preview, for the stub's records and tests.
pub fn semitone_ratio(semitones: i8) -> f64 {
    2f64.powf(f64::from(semitones) / 12.)
}

// ───────────────────────────── Library ─────────────────────────────

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

/// A sound that sounds like another, with how much (see `LibraryBridge::similar_sounds`).
#[derive(Clone, Debug, PartialEq)]
pub struct SimilarSound {
    pub item: LibraryItem,
    /// -1..1, about 1 for the same sound.
    pub similarity: f32,
}

/// A note played during a take.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RecNote {
    pub track: TrackId,
    pub key: u8,
    /// 0..=1.
    pub velocity: f32,
    /// On the timeline (inside the loop for a loop take).
    pub start_tick: Tick,
    pub length_ticks: Tick,
    /// Loop pass it was played in.
    pub pass: u32,
}

/// A finished MIDI take.
#[derive(Clone, Debug, PartialEq)]
pub struct MidiTake {
    /// The notes not handed out by [`EngineBridge::take_recorded_notes`] yet.
    pub notes: Vec<RecNote>,
    pub start_tick: Tick,
    pub end_tick: Tick,
    /// Loop passes it went through (1 = none).
    pub passes: u32,
}

/// Where recording stands.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RecordingState {
    /// A take is being captured right now.
    pub capturing: bool,
    /// Where it began.
    pub start_tick: Tick,
    pub pass: u32,
    /// The armed instrument tracks, which get the notes.
    pub tracks: Vec<TrackId>,
    /// The armed audio tracks with an input, which get a file each.
    pub audio_tracks: Vec<TrackId>,
    /// Takes started and fully drained since the engine started: a take is
    /// over (its files may still be arriving) when they are equal.
    pub takes_started: u64,
    pub takes_finished: u64,
    /// Pieces of audio and notes the engine could not queue since it started.
    pub dropped: u64,
}

/// Follows the engine's running count of lost pieces of a recording and says
/// how many the last take lost.
#[derive(Debug, Default)]
pub struct DropTracker {
    base: Option<u64>,
    was_capturing: bool,
    lost: u64,
}

impl DropTracker {
    /// Call every frame with whether a take is being captured and the engine's
    /// total. Returns what the latest take lost (0 once a new one begins).
    pub fn update(&mut self, capturing: bool, total: u64) -> u64 {
        if capturing {
            if !self.was_capturing {
                self.base = Some(total);
                self.lost = 0;
            }
        } else {
            // Losses while the take drains to disk count too.
            let base = *self.base.get_or_insert(total);
            self.lost = total.saturating_sub(base);
        }
        self.was_capturing = capturing;
        self.lost
    }
}

/// A WAV file written by an audio take.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioTake {
    pub track: TrackId,
    pub path: PathBuf,
    /// Timeline tick of its first frame.
    pub start_tick: Tick,
    pub frames: u64,
    pub sample_rate: u32,
    pub channels: u16,
    /// Set when the file could not be written (`frames` is what made it).
    pub error: Option<String>,
}

/// A MIDI input port as the Preferences page shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MidiPortState {
    /// Stable id, to pass to [`EngineBridge::set_midi_port_enabled`].
    pub id: String,
    pub name: String,
    /// The user's choice (on unless switched off).
    pub enabled: bool,
    /// Messages from this port reach the engine right now.
    pub open: bool,
}

/// What the audio engine reports about itself: contained panics, repaired
/// samples, devices silenced after a panic, and what the last compile had to
/// work around. Empty = all well.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EngineHealth {
    pub callback_panics: u64,
    pub node_panics: u64,
    /// Non-finite samples replaced by silence at the output.
    pub sanitized_samples: u64,
    /// Devices silenced after a panic, until the graph is rebuilt.
    pub crashed_devices: Vec<DeviceId>,
    /// `file:line` of the last contained panic.
    pub last_panic: Option<String>,
    /// Non-fatal problems of the last compile (missing plugin, bad route…).
    pub warnings: Vec<String>,
    /// Stretched renders that failed (newest last): those clips play in Re-Pitch.
    pub warp_failures: Vec<String>,
    /// Pieces of audio or notes the last recording could not keep.
    pub recording_lost: u64,
    /// The last compile failed: the previous graph keeps playing.
    pub error: Option<String>,
}

impl EngineHealth {
    pub fn is_clean(&self) -> bool {
        self.callback_panics == 0
            && self.node_panics == 0
            && self.sanitized_samples == 0
            && self.crashed_devices.is_empty()
            && self.warnings.is_empty()
            && self.warp_failures.is_empty()
            && self.recording_lost == 0
            && self.error.is_none()
    }

    /// Something stopped sound (a panic, a silenced device, a failed compile),
    /// as opposed to a warning about the project or a repaired sample.
    pub fn is_fault(&self) -> bool {
        self.callback_panics > 0 || self.node_panics > 0 || !self.crashed_devices.is_empty() || self.error.is_some()
    }

    /// One line per problem; `name_of` names a crashed device.
    pub fn lines(&self, name_of: impl Fn(DeviceId) -> Option<String>) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(e) = &self.error {
            out.push(format!("The project could not be compiled; the previous version keeps playing: {e}"));
        }
        for d in &self.crashed_devices {
            out.push(format!("{} crashed and is silent until the project is rebuilt", name_of(*d).unwrap_or_else(|| format!("Device {}", d.0))));
        }
        if self.callback_panics > 0 {
            out.push(format!("{} audio callback panic{} answered with silence", self.callback_panics, if self.callback_panics == 1 { "" } else { "s" }));
        }
        if self.sanitized_samples > 0 {
            out.push(format!("{} invalid sample{} (NaN or infinite) replaced with silence", self.sanitized_samples, if self.sanitized_samples == 1 { "" } else { "s" }));
        }
        if let Some(at) = &self.last_panic {
            out.push(format!("Last panic at {at}"));
        }
        if self.recording_lost > 0 {
            // A piece is about 5 ms of audio (256 frames) or one note event.
            out.push(format!(
                "The last recording lost {} piece{} of audio or notes (about {} ms of sound at most): the disk or the computer could not keep up",
                self.recording_lost,
                if self.recording_lost == 1 { "" } else { "s" },
                self.recording_lost * 256 * 1000 / 48_000
            ));
        }
        out.extend(self.warnings.iter().cloned());
        // The newest few: the list only grows.
        let shown = self.warp_failures.len().min(3);
        out.extend(self.warp_failures[self.warp_failures.len() - shown..].iter().map(|f| format!("A clip could not be time-stretched and plays in Re-Pitch: {f}")));
        if self.warp_failures.len() > shown {
            out.push(format!("…and {} more clips could not be time-stretched", self.warp_failures.len() - shown));
        }
        out
    }

    /// How many problems the status bar counts as warnings.
    pub fn warning_count(&self) -> usize {
        self.warnings.len() + self.warp_failures.len() + usize::from(self.recording_lost > 0)
    }
}

/// Remembers which problems the user was already told about, so each one
/// gets a toast the first time it appears and not on every frame.
#[derive(Debug, Default)]
pub struct NoticeLog {
    warnings: BTreeSet<String>,
    crashed: BTreeSet<DeviceId>,
    error: Option<String>,
    panicked: bool,
    sanitized: bool,
    warp_seen: usize,
    lost_seen: u64,
}

impl NoticeLog {
    /// The toasts for what is new in `h` since the last call. A problem that
    /// went away is forgotten, so it is told again if it comes back.
    pub fn fresh(&mut self, h: &EngineHealth) -> Vec<String> {
        let mut out = Vec::new();
        self.warnings.retain(|w| h.warnings.contains(w));
        for w in &h.warnings {
            if self.warnings.insert(w.clone()) {
                out.push(format!("Engine: {w}"));
            }
        }
        self.crashed.retain(|d| h.crashed_devices.contains(d));
        for d in &h.crashed_devices {
            if self.crashed.insert(*d) {
                out.push("A device crashed and was silenced; see the audio status in the status bar".to_owned());
            }
        }
        if h.error != self.error {
            if let Some(e) = &h.error {
                out.push(format!("The project could not be compiled: {e}"));
            }
            self.error = h.error.clone();
        }
        let panicked = h.callback_panics > 0 || h.node_panics > 0;
        if panicked && !self.panicked {
            out.push("The audio engine contained a crash and answered with silence".to_owned());
        }
        self.panicked = panicked;
        // Warp renders that failed: told when more of them appear.
        if h.warp_failures.len() > self.warp_seen {
            let n = h.warp_failures.len() - self.warp_seen;
            out.push(if n == 1 { "A clip could not be time-stretched and plays in Re-Pitch".to_owned() } else { format!("{n} clips could not be time-stretched and play in Re-Pitch") });
        }
        self.warp_seen = h.warp_failures.len();
        if h.recording_lost > self.lost_seen {
            out.push(format!("The recording lost audio: {} piece{} could not be kept (the disk or the computer could not keep up)", h.recording_lost, if h.recording_lost == 1 { "" } else { "s" }));
        }
        self.lost_seen = h.recording_lost;
        let sanitized = h.sanitized_samples > 0;
        if sanitized && !self.sanitized {
            out.push("Invalid audio (NaN) was replaced with silence".to_owned());
        }
        self.sanitized = sanitized;
        out
    }
}

/// How far a library sample's BPM can be trusted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BpmTrust {
    /// Set by the user, written in the file name or measured from the audio.
    #[default]
    Reliable,
    /// Guessed from the length of the file alone (assuming 4/4 and a whole
    /// number of bars): a loop warped to it can be badly out of tempo.
    Estimate,
}

impl BpmTrust {
    /// From the library's `bpm_source` column (`user`, `audio`, `filename`,
    /// `estimate`); a missing source (a tempo filled in from the analysis
    /// cache) is a measured one.
    pub fn from_source(source: Option<&str>) -> Self {
        match source {
            Some("estimate") => Self::Estimate,
            _ => Self::Reliable,
        }
    }

    /// Whether a loop may be warped to this BPM the moment it is dropped.
    pub fn allows_auto_warp(self) -> bool {
        self == Self::Reliable
    }
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
    /// Free-text search, optionally restricted to a key (compatible keys first).
    fn search(&self, query: &str, key: Option<Scale>) -> Vec<LibraryItem>;
    /// How far the BPM of sample `id` can be trusted (a loop whose BPM is
    /// only guessed from its length is not warped automatically).
    fn bpm_trust(&self, _id: u64) -> BpmTrust {
        BpmTrust::Reliable
    }
    /// Search for the Sounds panel: filters by type, key, tempo, tag, collection and favourites.
    fn search_sounds(&self, _q: &SoundQuery) -> SoundPage {
        SoundPage::default()
    }
    /// A search for the Sounds panel that can run on another thread, with connections
    /// of its own (`None` for bridges whose search is cheap enough for the UI thread).
    fn sound_searcher(&self) -> Option<SoundSearchFn> {
        None
    }
    /// The Splice download folder to index as a collection (`None` = none chosen
    /// and none found at Splice's default place). Cheap: call it as often as you like.
    fn set_splice_folder(&mut self, _folder: Option<PathBuf>) {}
    /// The free-plugin catalog with what each one is doing here. Cheap; refreshed
    /// about every two seconds, so a plugin the user installs by hand shows up.
    /// The demo library lists them all as it finds them (nothing installed).
    fn plugin_cards(&mut self) -> Vec<aitrack_library::plugins::PluginCard> {
        static DEMO: std::sync::OnceLock<Vec<aitrack_library::plugins::PluginCard>> = std::sync::OnceLock::new();
        DEMO.get_or_init(|| {
            let none = std::env::temp_dir().join("aitrack-demo-no-plugins");
            let dirs = aitrack_library::plugins::UserDirs { vst3: none.join("VST3"), clap: none.join("CLAP"), system: Vec::new() };
            aitrack_library::plugins::PluginInstaller::new(none, dirs).cards()
        })
        .clone()
    }
    /// Download and install plugin `id` (only when the user asked for it).
    fn install_plugin(&mut self, _id: &str) -> Result<(), String> {
        Err("The demo library cannot install plugins".into())
    }
    fn cancel_plugin_install(&mut self, _id: &str) {}
    /// Remove what AI-Track installed of `id`; the summary is for a toast.
    fn uninstall_plugin(&mut self, _id: &str) -> Result<String, String> {
        Err("Nothing installed by AI-Track".into())
    }
    /// Bundles installed since the last call: the plugin host scans exactly these.
    fn take_installed_plugins(&mut self) -> Vec<PathBuf> {
        Vec::new()
    }
    /// The sounds that sound most like sample `id`, nearest first, never `id` itself: the same
    /// kind of sound, closer when they share its key and tempo. Empty when the library has
    /// not listened to enough of its samples yet. May take a moment for a big library.
    fn similar_sounds(&self, _id: u64, _limit: usize) -> Vec<SimilarSound> {
        Vec::new()
    }
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
    items: Vec<LibraryItem>,
}

impl Default for StubLibrary {
    fn default() -> Self {
        let am = Scale::new(Key(9), ScaleKind::Minor);
        let cmaj = Scale::new(Key(0), ScaleKind::Major);
        let dm = Scale::new(Key(2), ScaleKind::Minor);
        let em = Scale::new(Key(4), ScaleKind::Minor);
        let fmaj = Scale::new(Key(5), ScaleKind::Major);
        let gm = Scale::new(Key(7), ScaleKind::Minor);
        type Row = (&'static str, &'static str, &'static str, Option<Scale>, Option<f32>, f32, bool);
        let rows: &[Row] = &[
            ("Kick Punchy 01", "drums/kicks", "Kick", Some(am), None, 0.42, false),
            ("Kick Deep Sub", "drums/kicks", "Kick", Some(dm), None, 0.61, false),
            ("Kick Analog 909", "drums/kicks", "Kick", None, None, 0.38, false),
            ("Kick Dark Rumble", "drums/kicks", "Kick", Some(am), None, 0.88, false),
            ("Kick Reggaeton Thump", "drums/kicks", "Kick", Some(gm), None, 0.45, false),
            ("Snare Crack", "drums/snares", "Snare", None, None, 0.31, false),
            ("Snare Room Tight", "drums/snares", "Snare", None, None, 0.27, false),
            ("Clap Wide Stack", "drums/claps", "Clap", None, None, 0.35, false),
            ("Clap Vinyl", "drums/claps", "Clap", None, None, 0.29, false),
            ("Hat Closed Tick", "drums/hats", "Hat", None, None, 0.08, false),
            ("Hat Open Sizzle", "drums/hats", "Hat", None, None, 0.52, false),
            ("Hat Loop 16th", "drums/hats", "Hat Loop", None, Some(128.), 7.5, true),
            ("Perc Dembow Loop", "drums/percussion", "Perc Loop", None, Some(95.), 10.1, true),
            ("Shaker Groove", "drums/percussion", "Perc Loop", None, Some(124.), 7.74, true),
            ("Bass Reese A", "bass/one-shots", "Bass", Some(am), None, 1.6, false),
            ("Bass Sub Pluck", "bass/one-shots", "Bass", Some(em), None, 0.9, false),
            ("Bass Rolling Loop Am", "bass/loops", "Bass Loop", Some(am), Some(128.), 7.5, true),
            ("Bass Offbeat Loop Dm", "bass/loops", "Bass Loop", Some(dm), Some(124.), 7.74, true),
            ("Lead Supersaw Am", "synths/leads", "Lead", Some(am), Some(128.), 3.75, true),
            ("Lead Square Hook", "synths/leads", "Lead", Some(cmaj), Some(120.), 4.0, true),
            ("Pad Warm Analog Am", "synths/pads", "Pad", Some(am), None, 6.2, false),
            ("Pad Glass F", "synths/pads", "Pad", Some(fmaj), None, 8.0, false),
            ("Pluck Bell C", "synths/plucks", "Pluck", Some(cmaj), None, 1.1, false),
            ("Pluck Chord Loop Am", "synths/plucks", "Pluck Loop", Some(am), Some(128.), 7.5, true),
            ("Vocal Chop Hey", "vocals/chops", "Vocal", Some(am), None, 0.6, false),
            ("Vocal Chop Ooh Gm", "vocals/chops", "Vocal", Some(gm), None, 1.2, false),
            ("Vocal Phrase Night", "vocals/phrases", "Vocal Loop", Some(am), Some(128.), 7.5, true),
            ("Riser White 8 Bars", "fx/risers", "Riser", None, Some(128.), 15.0, true),
            ("Impact Low Boom", "fx/impacts", "Impact", Some(am), None, 3.2, false),
            ("Atmos Rain Tape", "fx/atmos", "Atmos", None, None, 12.0, false),
        ];
        let items = rows
            .iter()
            .enumerate()
            .map(|(i, (name, cat, category, key, bpm, dur, is_loop))| LibraryItem {
                id: i as u64 + 1,
                name: (*name).into(),
                path: PathBuf::from(format!("Library/{cat}/{name}.wav")),
                category: (*category).into(),
                key: *key,
                bpm: *bpm,
                duration: *dur,
                is_loop: *is_loop,
                favorite: false,
            })
            .collect();
        Self { items }
    }
}

impl LibraryBridge for StubLibrary {
    fn search(&self, query: &str, key: Option<Scale>) -> Vec<LibraryItem> {
        let q = query.trim().to_lowercase();
        let mut out: Vec<LibraryItem> = self
            .items
            .iter()
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

    fn similar_sounds(&self, id: u64, limit: usize) -> Vec<SimilarSound> {
        let Some(me) = self.items.iter().find(|i| i.id == id) else { return Vec::new() };
        let words = |i: &LibraryItem| aitrack_library::text::words(&i.name).into_iter().collect::<std::collections::BTreeSet<_>>();
        let mine = words(me);
        let mut out: Vec<SimilarSound> = self
            .items
            .iter()
            .filter(|i| i.id != id && i.category.split_whitespace().next() == me.category.split_whitespace().next())
            .map(|i| {
                // A made-up closeness from the name, the kind of sound and the length.
                let shared = mine.intersection(&words(i)).count() as f32;
                let length = 1. / (1. + (i.duration - me.duration).abs());
                let kind = if i.is_loop == me.is_loop { 0.15 } else { 0. };
                let key = if i.key.is_some() && i.key == me.key { 0.1 } else { 0. };
                SimilarSound { item: i.clone(), similarity: (0.55 + 0.1 * shared + 0.2 * length + kind + key).min(0.99) }
            })
            .collect();
        out.sort_by(|a, b| b.similarity.total_cmp(&a.similarity).then_with(|| a.item.name.cmp(&b.item.name)));
        out.truncate(limit);
        out
    }

    fn search_sounds(&self, q: &SoundQuery) -> SoundPage {
        let words = aitrack_library::text::words(&q.text);
        let folder_of = |i: &LibraryItem| i.path.parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let origin_of = |id: u64| match id % 5 {
            0 => SoundOrigin::Splice,
            1 | 2 => SoundOrigin::Factory,
            _ => SoundOrigin::Mine,
        };
        let group_of = |i: &LibraryItem| {
            let c = i.category.to_lowercase();
            let any = |words: &[&str]| words.iter().any(|w| c.contains(w));
            if any(&["kick", "snare", "clap", "hat", "perc"]) {
                SoundGroup::Drums
            } else if any(&["bass"]) {
                SoundGroup::Bass
            } else if any(&["vocal"]) {
                SoundGroup::Vocals
            } else if any(&["riser", "impact", "atmos", "fx"]) {
                SoundGroup::Fx
            } else if any(&["lead", "pad", "pluck"]) {
                SoundGroup::Synths
            } else {
                SoundGroup::Instruments
            }
        };
        let tag_of = |i: &LibraryItem| if i.is_loop { "loop" } else { "one-shot" }.to_owned();
        let fits_key = |sample: Option<Scale>, (song, exact): (Scale, bool)| match sample {
            None => true,
            Some(k) if exact => k == song,
            Some(k) => {
                k == song
                    || k.mask() == song.mask()
                    || k.camelot().zip(song.camelot()).is_some_and(|(a, b)| aitrack_library::query::camelot_neighbors(&b).contains(&a))
            }
        };
        let mut hits: Vec<SoundHit> = self
            .items
            .iter()
            .filter(|i| aitrack_library::text::score(&words, &i.name, &[&i.category]).is_some())
            .filter(|i| q.folder.as_ref().is_none_or(|f| f.under.as_deref() == Some(folder_of(i).as_str())))
            .filter(|i| match q.kind {
                SoundKind::All => true,
                SoundKind::Loops => i.is_loop,
                SoundKind::OneShots => !i.is_loop,
            })
            .filter(|i| q.group.is_none_or(|g| group_of(i) == g))
            .filter(|i| q.key.is_none_or(|k| fits_key(i.key, k)))
            .filter(|i| {
                q.bpm.is_none_or(|(bpm, tol)| {
                    !i.is_loop
                        || i.bpm.is_some_and(|b| [0.5f32, 1., 2.].iter().any(|f| ((b * f) - bpm).abs() <= bpm * tol / 100.))
                })
            })
            .filter(|i| q.tag.as_ref().is_none_or(|t| &tag_of(i) == t))
            .filter(|i| q.origin.is_none_or(|o| origin_of(i.id) == o))
            .filter(|i| !q.favorites || i.favorite)
            .map(|i| SoundHit { item: i.clone(), origin: origin_of(i.id), tags: vec![tag_of(i)], bpm_trust: BpmTrust::Reliable, unanalysed: false })
            .collect();
        hits.sort_by(|a, b| a.item.name.cmp(&b.item.name));
        let mut tags: BTreeMap<String, usize> = BTreeMap::new();
        for h in &hits {
            for t in &h.tags {
                *tags.entry(t.clone()).or_default() += 1;
            }
        }
        let mut tags: Vec<(String, usize)> = tags.into_iter().collect();
        tags.sort_by_key(|(t, n)| (std::cmp::Reverse(*n), t.clone()));
        let mut folders: BTreeMap<String, u64> = BTreeMap::new();
        for i in &self.items {
            *folders.entry(folder_of(i)).or_default() += 1;
        }
        let mut folders: Vec<SoundFolder> =
            folders.into_iter().map(|(label, count)| SoundFolder { library: 1, root: 1, under: Some(label.clone()), label, count }).collect();
        folders.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.label.cmp(&b.label)));
        SoundPage { hits, more: false, tags, folders, splice: SpliceState::Indexed { folder: PathBuf::from("~/Splice/sounds"), sounds: 6 } }
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
    DrumsReady { job: JobId, result: DrumExtraction },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DrumExtractMode {
    #[default]
    Auto,
    PureDsp,
    DemucsHybrid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DrumClassTag {
    Kick,
    Snare,
    Clap,
    ClosedHat,
    OpenHat,
    Tom,
    Perc,
    Crash,
}

impl DrumClassTag {
    pub const ALL: [Self; 8] = [Self::Kick, Self::Snare, Self::Clap, Self::ClosedHat, Self::OpenHat, Self::Tom, Self::Perc, Self::Crash];

    pub fn label(self) -> &'static str {
        match self {
            Self::Kick => "Kick",
            Self::Snare => "Snare",
            Self::Clap => "Clap",
            Self::ClosedHat => "Hat",
            Self::OpenHat => "Open Hat",
            Self::Tom => "Tom",
            Self::Perc => "Perc",
            Self::Crash => "Crash",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct DrumOneShot {
    pub id: usize,
    pub name: String,
    pub class: DrumClassTag,
    pub time_sec: f64,
    pub duration_sec: f64,
    pub peak_db: f32,
    pub isolation_score: f32,
    pub path: PathBuf,
    pub peaks: Vec<f32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DrumLoop {
    pub name: String,
    pub bpm: f32,
    pub bars: u8,
    pub start_sec: f64,
    pub duration_sec: f64,
    pub isolation_score: f32,
    pub path: PathBuf,
    pub peaks: Vec<f32>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DrumExtraction {
    pub source_path: PathBuf,
    pub source_name: String,
    pub engine_used: String,
    pub sample_rate: u32,
    pub duration_sec: f64,
    pub bpm: Option<f32>,
    pub clean_stem_path: PathBuf,
    pub oneshots: Vec<DrumOneShot>,
    pub loops: Vec<DrumLoop>,
    pub out_dir: PathBuf,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DrumExtractRequest {
    pub path: PathBuf,
    pub engine: DrumExtractMode,
    pub de_bleed: f32,
    pub de_reverb: f32,
    pub transient: f32,
    pub sensitivity: f32,
    pub loop_bars: u8,
    pub normalize_db: Option<f32>,
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

/// A coding agent the user has installed, used as the assistant's brain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentKind {
    ClaudeCode,
    Codex,
}

impl AgentKind {
    pub const ALL: [AgentKind; 2] = [Self::ClaudeCode, Self::Codex];

    pub fn name(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex",
        }
    }

    /// How to get it, shown when it is not installed.
    pub fn setup(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Install Claude Code (npm install -g @anthropic-ai/claude-code), then run `claude` once in a terminal and sign in with your own plan.",
            Self::Codex => "Install the Codex CLI (npm install -g @openai/codex), then run `codex login` in a terminal and sign in with your own account.",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentStatus {
    pub kind: AgentKind,
    pub installed: bool,
    /// The CLI's version line, empty when it is not installed.
    pub version: String,
}

pub trait AiBridge {
    /// Audio to MIDI; notes are timed at `bpm`.
    fn extract_midi(&mut self, path: &Path, mode: MidiMode, bpm: f64) -> JobId;
    fn split_stems(&mut self, path: &Path) -> JobId;
    /// Extract clean isolated drum one-shots (one by one) and seamless loops.
    fn extract_drums(&mut self, req: DrumExtractRequest) -> JobId;
    /// The stem model's licence notice while it still has to be accepted.
    fn stem_license(&self) -> Option<ModelNotice> {
        None
    }
    fn accept_license(&mut self, _model: &str) -> Result<(), String> {
        Ok(())
    }
    /// Key / BPM / format analysis of an audio file.
    fn analyze(&mut self, path: &Path) -> JobId;
    /// The agent CLIs found on this machine (empty until detection finishes).
    fn agents(&self) -> Vec<AgentStatus> {
        Vec::new()
    }
    /// Look for the agent CLIs again (after the user installs one).
    fn refresh_agents(&mut self) {}
    /// The agent that answers the chat; `None` is the built-in assistant.
    fn agent(&self) -> Option<AgentKind> {
        None
    }
    fn set_agent(&mut self, _agent: Option<AgentKind>) {}
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
    Drums(DrumExtractRequest),
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

    fn extract_drums(&mut self, req: DrumExtractRequest) -> JobId {
        if req.path.as_os_str().is_empty() {
            return self.push(StubJobKind::Fail("no audio file".into()));
        }
        self.push(StubJobKind::Drums(req))
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
                StubJobKind::Drums(_) => 1.2,
                StubJobKind::Chat(..) => 0.9,
                StubJobKind::Fail(_) | StubJobKind::Analyze(_) => 0.3,
            };
            let f = (secs / total).min(1.);
            if f < 1. {
                if f - job.reported >= 0.04 && matches!(job.kind, StubJobKind::Midi(_) | StubJobKind::Stems(_) | StubJobKind::Drums(_)) {
                    job.reported = f;
                    let label = match job.kind {
                        StubJobKind::Midi(_) => "Extracting MIDI",
                        StubJobKind::Drums(_) => "Isolating clean drums",
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
                StubJobKind::Drums(req) => {
                    let source_name = req.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Track".into());
                    let mk_peaks = |n: usize| (0..n).map(|i| (1.0 - i as f32 / n as f32).powi(2)).collect();
                    out.push(AiEvent::DrumsReady {
                        job: job.id,
                        result: DrumExtraction {
                            source_path: req.path.clone(),
                            source_name: source_name.clone(),
                            engine_used: "RX HPSS + Spectral De-Bleed (stub)".into(),
                            sample_rate: 44_100,
                            duration_sec: 4.0,
                            bpm: Some(120.0),
                            clean_stem_path: req.path.with_extension("clean_drums.wav"),
                            oneshots: vec![
                                DrumOneShot { id: 1, name: "Kick 01".into(), class: DrumClassTag::Kick, time_sec: 0.0, duration_sec: 0.24, peak_db: -1.0, isolation_score: 0.94, path: req.path.with_extension("kick_01.wav"), peaks: mk_peaks(32) },
                                DrumOneShot { id: 2, name: "Snare 01".into(), class: DrumClassTag::Snare, time_sec: 0.5, duration_sec: 0.22, peak_db: -1.0, isolation_score: 0.91, path: req.path.with_extension("snare_01.wav"), peaks: mk_peaks(32) },
                                DrumOneShot { id: 3, name: "Closed Hat 01".into(), class: DrumClassTag::ClosedHat, time_sec: 0.25, duration_sec: 0.09, peak_db: -1.0, isolation_score: 0.89, path: req.path.with_extension("hat_01.wav"), peaks: mk_peaks(32) },
                            ],
                            loops: vec![DrumLoop {
                                name: format!("{source_name} Drums {}B 120BPM (Main)", req.loop_bars),
                                bpm: 120.0,
                                bars: req.loop_bars,
                                start_sec: 0.0,
                                duration_sec: req.loop_bars as f64 * 2.0,
                                isolation_score: 0.92,
                                path: req.path.with_extension("drums_loop.wav"),
                                peaks: mk_peaks(64),
                            }],
                            out_dir: req.path.parent().unwrap_or(Path::new(".")).to_path_buf(),
                        },
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

    #[test]
    fn the_stub_meters_of_the_dynamics_family_are_as_long_as_their_editors_read() {
        // What each editor needs before it treats the meter as live, and what the window frame reads.
        let need = [
            ("fx.compressor", 70),
            ("fx.buscomp", 70),
            ("fx.clipper", 3),
            ("fx.gate", 3),
            ("fx.limiter", 7),
            ("fx.mbdyn", aitrack_stock::fx::mbdyn::METER_LEN),
            ("fx.drumbus", 135),
            ("fx.tape", 3),
            ("fx.amp", 3),
            ("fx.console", 3),
        ];
        for (plugin, len) in need {
            let mut m = [0.0f32; 320];
            assert_eq!(stub_device_meter(plugin, true, 0.1, 1., &mut m), len, "{plugin}");
            assert!(m[..len].iter().all(|v| v.is_finite()), "{plugin}");
            // Stopped, nothing is published as a level.
            assert_eq!(stub_device_meter(plugin, false, 0., 0., &mut m), len, "{plugin} stopped");
        }
    }
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
    fn engine_problems_are_told_once_and_again_if_they_return() {
        let mut log = NoticeLog::default();
        let mut h = EngineHealth::default();
        assert!(h.is_clean() && log.fresh(&h).is_empty());
        h.warnings = vec!["Plugin Foo is missing".into()];
        assert!(!h.is_clean() && !h.is_fault(), "a warning is not a fault");
        assert_eq!(log.fresh(&h), vec!["Engine: Plugin Foo is missing".to_owned()]);
        assert!(log.fresh(&h).is_empty(), "not again while it stays");
        h.warnings.push("A send points nowhere".into());
        assert_eq!(log.fresh(&h).len(), 1, "only the new one");
        h.warnings.clear();
        assert!(log.fresh(&h).is_empty());
        h.warnings = vec!["Plugin Foo is missing".into()];
        assert_eq!(log.fresh(&h).len(), 1, "it came back");
        // Warp renders that failed are told as they come and listed, but are not a fault.
        h.warp_failures = vec!["pad.wav: boom".into()];
        assert!(!h.is_clean() && !h.is_fault());
        assert_eq!(h.warning_count(), 2);
        assert_eq!(log.fresh(&h), vec!["A clip could not be time-stretched and plays in Re-Pitch".to_owned()]);
        assert!(log.fresh(&h).is_empty());
        h.warp_failures.push("lead.wav: boom".into());
        assert!(log.fresh(&h)[0].starts_with("A clip"));
        assert!(h.lines(|_| None).iter().any(|l| l.contains("pad.wav")));
        h.warp_failures.clear();
        // A recording that lost audio says so once it is over, with how much.
        h.recording_lost = 7;
        assert!(!h.is_fault() && !h.is_clean());
        let told = log.fresh(&h);
        assert_eq!(told.len(), 1);
        assert!(told[0].contains("7 pieces"), "{told:?}");
        assert!(h.lines(|_| None).iter().any(|l| l.contains("7 pieces")));
        assert!(log.fresh(&h).is_empty());
        h.recording_lost = 0;
        // Faults.
        h.crashed_devices = vec![DeviceId(7)];
        h.node_panics = 1;
        h.sanitized_samples = 3;
        h.error = Some("no Main track".into());
        assert!(h.is_fault());
        assert_eq!(log.fresh(&h).len(), 4, "crashed device, compile error, panic, invalid audio");
        assert!(log.fresh(&h).is_empty());
        let lines = h.lines(|d| (d.0 == 7).then(|| "EQ Eight".to_owned()));
        assert!(lines.iter().any(|l| l.starts_with("EQ Eight crashed")));
        assert!(lines.iter().any(|l| l.contains("3 invalid samples")));
        assert!(lines.iter().any(|l| l.contains("no Main track")));
        assert!(lines.contains(&"Plugin Foo is missing".to_owned()));
    }

    #[test]
    fn the_losses_of_a_recording_are_counted_for_that_take_only() {
        let mut t = DropTracker::default();
        assert_eq!(t.update(false, 5), 0, "drops before any take are not a take's");
        assert_eq!(t.update(true, 5), 0);
        assert_eq!(t.update(true, 9), 0, "told once it is over, not while it runs");
        assert_eq!(t.update(false, 12), 7, "the tail that drained to disk counts too");
        assert_eq!(t.update(false, 12), 7);
        assert_eq!(t.update(true, 12), 0, "a new take starts from nothing");
        assert_eq!(t.update(false, 12), 0);
    }

    #[test]
    fn the_stub_bridge_lists_midi_ports_and_remembers_the_choices() {
        let mut e = StubEngine::default();
        assert!(e.midi_ports().iter().all(|p| p.enabled && p.open), "every port on by default");
        e.set_midi_port_enabled("stub-pads", false);
        let ports = e.midi_ports();
        assert_eq!(ports.iter().filter(|p| p.enabled).count(), 1);
        assert!(ports.iter().find(|p| p.id == "stub-pads").is_some_and(|p| !p.open));
        e.set_midi_target(Some(TrackId(3)));
        assert_eq!(e.midi_target(), Some(TrackId(3)));
        e.set_midi_target(None);
        assert_eq!(e.midi_target(), None, "None = the armed tracks");
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
    fn library_filters_by_key() {
        let lib = StubLibrary::default();
        let am = Scale::new(Key(9), ScaleKind::Minor);
        let found = lib.search("", Some(am));
        assert!(found.iter().all(|i| i.key.is_none_or(|k| k.mask() == am.mask())));
        assert!(lib.search("kick", None).iter().all(|i| i.name.to_lowercase().contains("kick") || i.category == "Kick"));
    }

    #[test]
    fn the_stub_finds_similar_sounds_of_the_same_kind() {
        let lib = StubLibrary::default();
        let kick = lib.search("Kick Punchy", None).remove(0);
        let similar = lib.similar_sounds(kick.id, 10);
        assert_eq!(similar.len(), 4, "the other four kicks");
        assert!(similar.iter().all(|s| s.item.category == "Kick" && s.item.id != kick.id));
        assert!(similar.windows(2).all(|w| w[0].similarity >= w[1].similarity));
        assert_eq!(lib.similar_sounds(kick.id, 2).len(), 2);
        assert!(lib.similar_sounds(9_999, 5).is_empty());
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
