//! Public engine API: [`Engine::new`] spawns the control and graveyard
//! threads and (optionally) opens the audio device; [`EngineHandle`] is the
//! cheap, thread-safe remote the UI holds.
//!
//! Threads:
//! * **UI / caller** — calls [`EngineHandle`] methods. Project snapshots go
//!   to the control thread over a channel; transport commands go straight
//!   to the audio thread through an `rtrb` queue.
//! * **control** (`aitrack-engine-control`) — compiles graphs, keeps the
//!   processor pool and sample cache, owns the audio stream and MIDI ports,
//!   reopens the device when it is lost or the system default output
//!   changes (see [`DeviceWatch`]), recompiles when plugin latency
//!   changes or samples finish loading.
//! * **graveyard** (`aitrack-engine-graveyard`) — drops retired graphs (and
//!   with them, possibly the last reference to a plugin) off the audio and
//!   control threads.
//! * **audio** — the cpal callback running [`AudioProcessor`].

use crate::audio_io::{self, ActiveAudio, AudioConfig, AudioDeviceInfo, ChannelSelection, OpenStream};
use crate::compile::{compile, CompileContext};
use crate::factory::ProcessorFactory;
use crate::graph::CompiledGraph;
use crate::record::{AudioTake, MidiCollector, MidiTake, RecordedNote, RecordingInfo, Recorder, AUDIO_QUEUE, MIDI_QUEUE as REC_MIDI_QUEUE};
use crate::midi_io::{self, MidiConnection, MidiPortInfo, MidiPortStatus};
use crate::rt::{AudioProcessor, InputQueue, InputSink, InputStamp, MidiMessage, RtCommand, RtQueues, TimedMidi, TransportInit};
use crate::sample_cache::SampleCache;
use crate::shared::{ProcessorPool, SharedProcessor};
use crate::status::{AudioHealth, ClipSlotStatus, LatencyReport, MeterReading, PlayheadAnchor, SharedStatus};
use crate::tap::{DeviceMeter, TapReader};
use crate::EngineError;
use aitrack_model::{DeviceId, Monitor, Project, Quantize, SceneId, Tick, TrackId};
use crossbeam_channel::{bounded, unbounded, Receiver, RecvTimeoutError, Sender};
use rtrb::{Consumer, Producer, RingBuffer};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Number of independent live-MIDI queues (open ports + the virtual one).
pub const MIDI_QUEUES: usize = 8;
const GRAPH_QUEUE: usize = 8;
const COMMAND_QUEUE: usize = 1024;
const MIDI_QUEUE: usize = 1024;

/// Engine start-up settings.
#[derive(Clone, Debug)]
pub struct EngineConfig {
    /// Audio device to open; `None` runs headless and [`Engine::new`]
    /// returns the [`AudioProcessor`] for the caller to drive.
    pub audio: Option<AudioConfig>,
    /// Sample rate when headless.
    pub headless_sample_rate: f64,
    /// Largest internal block; device callbacks larger than this are split.
    pub max_block: usize,
    /// MIDI input port ids to open at start.
    pub midi_inputs: Vec<String>,
    /// Count-in bars before playback (0 = off).
    pub count_in_bars: u32,
    /// Open the device on the control thread instead of inside
    /// [`Engine::new`], so a slow driver (Bluetooth, a sleeping USB box)
    /// cannot stall the caller. Watch [`EngineHandle::audio_generation`]
    /// and [`EngineHandle::last_error`] for the outcome.
    pub open_in_background: bool,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            audio: Some(AudioConfig::default()),
            headless_sample_rate: 48_000.0,
            max_block: 1024,
            midi_inputs: Vec::new(),
            count_in_bars: 0,
            open_in_background: false,
        }
    }
}

impl EngineConfig {
    pub fn headless(sample_rate: f64) -> Self {
        Self { audio: None, headless_sample_rate: sample_rate, ..Self::default() }
    }
}

enum Msg {
    Project(Arc<Project>),
    Recompile,
    SetAudio(Option<AudioConfig>, Sender<Result<Option<ActiveAudio>, EngineError>>),
    OpenMidi(String, Sender<Result<(), EngineError>>),
    CloseMidi(String),
    Flush(Sender<()>),
    /// An audio track's input channels (`None` = default input).
    TrackInput(TrackId, Option<(u16, u16)>),
    /// An audio track's monitor mode (`None` = the project's).
    TrackMonitor(TrackId, Option<Monitor>),
    /// An audio track without any input.
    TrackNoInput(TrackId, bool),
    /// Tests: rebuild the audio thread as a device reopen does (headless).
    #[cfg(test)]
    TestRebuild(Sender<AudioProcessor>),
    /// A macro moved: `(rack, index, value)`.
    Macro(DeviceId, u16, f32),
    /// A fast-path edit also made to the control thread's copy of the project.
    Patch(crate::compile::Patch),
    Shutdown,
}

/// State shared between the handle and the control thread.
struct HandleShared {
    commands: Mutex<Option<Producer<RtCommand>>>,
    virtual_midi: Mutex<Option<Producer<TimedMidi>>>,
    last_error: Mutex<Option<String>>,
    warnings: Mutex<Vec<String>>,
    active_audio: Mutex<Option<ActiveAudio>>,
    config: Mutex<Option<AudioConfig>>,
    /// Bumped every time the device is opened, reopened or closed.
    generation: AtomicU64,
    /// Bumped every time a compile finishes (with a graph or with an error),
    /// after `warnings` and `last_error` have been updated.
    compile_generation: AtomicU64,
    /// Graphs compiled so far (successful or not).
    compile_count: AtomicU64,
    /// Project snapshots absorbed as parameter patches, without a compile.
    patch_count: AtomicU64,
    /// Ids of the MIDI inputs that are open.
    midi_open: Mutex<Vec<String>>,
    /// MIDI notes recorded by the audio thread, paired up for the app.
    rec_midi: Mutex<MidiCollector>,
    /// Takes already handed out by `finish_midi_recording`.
    rec_midi_taken: AtomicU64,
    /// Writes the WAV files of audio takes.
    recorder: Arc<Recorder>,
    /// Headless engines: where `feed_input` puts the synthetic input.
    virtual_input: Mutex<Option<InputSink>>,
}

#[derive(Clone, Copy, Debug, Default)]
struct TrackIo {
    input: Option<(u16, u16)>,
    monitor: Option<Monitor>,
    none: bool,
}

/// The input queue of a runner: samples, their capture stamps, channels, selection.
type InputLink = (Consumer<f32>, Consumer<InputStamp>, usize, usize, usize);

/// An input queue for `ch` interleaved channels: about half a second of
/// samples and room for a stamp per callback.
fn input_ring(rate: usize, ch: usize) -> (InputSink, Consumer<f32>, Consumer<InputStamp>) {
    let (samples, rx) = RingBuffer::new((rate / 2).max(4096) * ch);
    let (stamps_tx, stamps) = RingBuffer::new(256);
    (InputSink { samples, stamps: stamps_tx, written: 0 }, rx, stamps)
}

/// Engine entry point.
pub struct Engine;

impl Engine {
    #[allow(clippy::new_ret_no_self)]
    /// Start the engine. With `config.audio = Some(..)` the device is opened
    /// and the second value is `None`; headless, it is the processor to
    /// drive by hand (tests, custom backends).
    pub fn new(
        config: EngineConfig,
        factory: Arc<dyn ProcessorFactory>,
    ) -> Result<(EngineHandle, Option<AudioProcessor>), EngineError> {
        let status = Arc::new(SharedStatus::new());
        let shared = Arc::new(HandleShared {
            commands: Mutex::new(None),
            virtual_midi: Mutex::new(None),
            last_error: Mutex::new(None),
            warnings: Mutex::new(Vec::new()),
            active_audio: Mutex::new(None),
            config: Mutex::new(config.audio.clone()),
            generation: AtomicU64::new(0),
            compile_generation: AtomicU64::new(0),
            compile_count: AtomicU64::new(0),
            patch_count: AtomicU64::new(0),
            midi_open: Mutex::new(Vec::new()),
            rec_midi: Mutex::new(MidiCollector::default()),
            rec_midi_taken: AtomicU64::new(0),
            recorder: Arc::new(Recorder::spawn().map_err(EngineError::Io)?),
            virtual_input: Mutex::new(None),
        });
        let (tx, rx) = unbounded::<Msg>();
        let (grave_tx, grave_rx) = unbounded::<Box<CompiledGraph>>();
        let graveyard = std::thread::Builder::new()
            .name("aitrack-engine-graveyard".into())
            .spawn(move || {
                while let Ok(g) = grave_rx.recv() {
                    drop(g);
                }
            })
            .map_err(EngineError::Io)?;

        let loader_tx = tx.clone();
        let samples = SampleCache::with_loader(move || {
            let _ = loader_tx.send(Msg::Recompile);
        });
        let handle_samples = samples.clone();
        let mut control = Control {
            factory,
            pool: ProcessorPool::new(),
            samples,
            status: status.clone(),
            shared: shared.clone(),
            project: None,
            sample_rate: config.headless_sample_rate,
            max_frames: config.max_block.clamp(32, 8192),
            graph_tx: None,
            retire_rx: None,
            pending: None,
            graveyard: grave_tx,
            stream: None,
            audio: config.audio.clone(),
            midi_ports: config.midi_inputs.clone(),
            midi_conns: Vec::new(),
            midi_spare: Vec::new(),
            compile_gen: 0,
            compiled: None,
            track_io: std::collections::HashMap::new(),
            assumed_latency: Vec::new(),
            dirty: false,
            count_in: config.count_in_bars,
            watch: DeviceWatch::default(),
            seen_callback_panics: 0,
            panic_seen_at: None,
        };
        let manual = match &config.audio {
            // `check_device` opens it on the control thread's first tick.
            Some(_) if config.open_in_background => None,
            Some(cfg) => {
                if let Err(e) = control.open_audio(cfg.clone()) {
                    log::error!("could not open audio device: {e}");
                    *shared.last_error.lock().unwrap_or_else(|p| p.into_inner()) = Some(e.to_string());
                }
                None
            }
            None => Some(control.headless_runner(config.headless_sample_rate)),
        };
        let thread = std::thread::Builder::new()
            .name("aitrack-engine-control".into())
            .spawn(move || control.run(rx))
            .map_err(EngineError::Io)?;
        Ok((
            EngineHandle { tx, shared, status, samples: handle_samples, thread: Some(thread), _graveyard: Some(graveyard) },
            manual,
        ))
    }
}

/// Thread-safe remote control of the engine. Dropping it shuts the engine
/// down (stops audio, joins the control thread).
pub struct EngineHandle {
    tx: Sender<Msg>,
    shared: Arc<HandleShared>,
    status: Arc<SharedStatus>,
    samples: SampleCache,
    thread: Option<JoinHandle<()>>,
    _graveyard: Option<JoinHandle<()>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl EngineHandle {
    fn send_rt(&self, cmd: RtCommand) -> bool {
        lock(&self.shared.commands).as_mut().is_some_and(|p| p.push(cmd).is_ok())
    }

    /// Hand the engine a new project snapshot. Compiled off this thread;
    /// devices whose id and kind did not change keep their processors.
    /// When the snapshot differs from the running graph only by device
    /// parameters, track volume / pan, send levels or fields the engine does
    /// not read (names, colours, heights…), it is applied as parameter
    /// updates to the running graph and nothing is compiled
    /// ([`compile_count`](Self::compile_count) does not move).
    pub fn update_project(&self, project: &Project) {
        let _ = self.tx.send(Msg::Project(Arc::new(project.clone())));
    }

    /// Like [`update_project`](Self::update_project) without cloning.
    pub fn update_project_arc(&self, project: Arc<Project>) {
        let _ = self.tx.send(Msg::Project(project));
    }

    /// Block until every message sent so far is processed and the
    /// resulting graph is queued for the audio thread.
    pub fn flush(&self) {
        let (tx, rx) = bounded(1);
        if self.tx.send(Msg::Flush(tx)).is_ok() {
            let _ = rx.recv_timeout(Duration::from_secs(10));
        }
    }

    pub fn play(&self) {
        self.send_rt(RtCommand::Play);
    }

    /// Stop and return to where playback started (stop again: go to 0).
    /// Also stops Session clips.
    pub fn stop(&self) {
        self.send_rt(RtCommand::Stop);
    }

    /// Stop in place.
    pub fn pause(&self) {
        self.send_rt(RtCommand::Pause);
    }

    pub fn seek(&self, tick: Tick) {
        self.send_rt(RtCommand::Seek(tick));
    }

    /// Loop between two ticks, or `None` to stop looping.
    pub fn set_loop(&self, range: Option<(Tick, Tick)>) {
        self.send_rt(RtCommand::SetLoop(range));
    }

    pub fn set_metronome(&self, on: bool) {
        self.send_rt(RtCommand::SetMetronome(on));
    }

    pub fn toggle_metronome(&self) -> bool {
        let on = !self.status.metronome.load(Ordering::Relaxed);
        self.status.metronome.store(on, Ordering::Relaxed);
        self.send_rt(RtCommand::SetMetronome(on));
        on
    }

    /// Bars of metronome count-in before playback starts (0 = off).
    pub fn set_count_in(&self, bars: u32) {
        self.send_rt(RtCommand::SetCountIn(bars));
    }

    /// Record flag passed to processors through the transport (recording
    /// itself is a later step).
    pub fn set_recording(&self, on: bool) {
        self.send_rt(RtCommand::SetRecording(on));
    }

    /// Launch the Session clip in `(track, scene)` at its launch quantize.
    pub fn launch_clip(&self, track: TrackId, scene: SceneId) {
        self.send_rt(RtCommand::LaunchClip(track, scene));
    }

    pub fn launch_scene(&self, scene: SceneId) {
        self.send_rt(RtCommand::LaunchScene(scene));
    }

    pub fn stop_track(&self, track: TrackId) {
        self.send_rt(RtCommand::StopTrack(track));
    }

    pub fn stop_all_clips(&self) {
        self.send_rt(RtCommand::StopAllClips);
    }

    /// Stop Session clips and let every track follow the Arrangement again.
    pub fn back_to_arrangement(&self) {
        self.send_rt(RtCommand::BackToArrangement);
    }

    /// Quantize for scene launches into empty slots and track stops.
    pub fn set_global_quantize(&self, q: Quantize) {
        self.send_rt(RtCommand::SetGlobalQuantize(q));
    }

    /// Route live MIDI to one track; `None` = every armed instrument track.
    pub fn set_midi_target(&self, track: Option<TrackId>) {
        self.status.midi_target.store(track.map_or(u64::MAX, |t| t.0), Ordering::Relaxed);
        self.send_rt(RtCommand::SetMidiTarget(track));
    }

    /// The track live MIDI goes to (`None` = every armed instrument track).
    pub fn midi_target(&self) -> Option<TrackId> {
        Some(self.status.midi_target.load(Ordering::Relaxed)).filter(|v| *v != u64::MAX).map(TrackId)
    }

    /// Live MIDI messages the audio thread has read since the engine
    /// started (every port and the on-screen keyboard): an activity light
    /// for the MIDI preferences.
    pub fn midi_message_count(&self) -> u64 {
        self.status.midi_messages.load(Ordering::Relaxed)
    }

    /// Bytes of the sample cache held by warp renders (part of its total).
    pub fn sample_cache_warp_bytes(&self) -> usize {
        self.samples.warp_bytes()
    }

    /// The warp renders that failed (their clips keep playing in Re-Pitch).
    pub fn sample_cache_warp_failures(&self) -> Vec<String> {
        self.samples.warp_failures()
    }

    /// Every MIDI input port of the system, with whether it is open.
    pub fn midi_ports(&self) -> Vec<MidiPortStatus> {
        let open = lock(&self.shared.midi_open).clone();
        midi_io::midi_inputs().into_iter().map(|p| MidiPortStatus { open: open.contains(&p.id), id: p.id, name: p.name }).collect()
    }

    /// Ids of the MIDI inputs that are open (even if the port has since
    /// disappeared from the system).
    pub fn open_midi_inputs(&self) -> Vec<String> {
        lock(&self.shared.midi_open).clone()
    }

    #[cfg(test)]
    pub(crate) fn test_rebuild(&self) -> AudioProcessor {
        let (tx, rx) = bounded(1);
        self.tx.send(Msg::TestRebuild(tx)).unwrap();
        rx.recv_timeout(Duration::from_secs(5)).unwrap()
    }

    #[cfg(test)]
    pub(crate) fn test_status(&self) -> &Arc<SharedStatus> {
        &self.status
    }

    // ───────────── Recording ─────────────

    /// Where recording stands.
    pub fn recording_info(&self) -> RecordingInfo {
        let st = &self.status;
        RecordingInfo {
            armed: st.recording.load(Ordering::Relaxed),
            capturing: st.rec_capturing.load(Ordering::Acquire),
            start_tick: f64::from_bits(st.rec_start_tick.load(Ordering::Relaxed)),
            pass: st.rec_pass.load(Ordering::Relaxed),
            takes_started: st.rec_sessions.load(Ordering::Acquire),
            takes_finished: st.rec_ended.load(Ordering::Acquire),
            midi_tracks: lock(&st.armed_midi).clone(),
            audio_tracks: lock(&st.armed_audio).clone(),
            dropped: st.rec_dropped.load(Ordering::Relaxed),
            input_dropped_frames: st.input_dropped.load(Ordering::Relaxed),
            input_underrun_frames: st.input_underruns.load(Ordering::Relaxed),
        }
    }

    /// The notes of the take that are complete so far (note-on and note-off
    /// both seen), in start order; the take goes on and each note is handed
    /// out once. Call it now and then during a take so the queue never fills
    /// (it holds thousands of events, minutes of playing).
    pub fn take_recorded_notes(&self) -> Vec<RecordedNote> {
        lock(&self.shared.rec_midi).take_closed()
    }

    /// The finished MIDI take: the notes not handed out yet (notes still held
    /// when it stopped end there), its start and end tick and how many loop
    /// passes it went through. `None` while a take is still being captured
    /// or draining, or when no new take ended since the last call.
    pub fn finish_midi_recording(&self) -> Option<MidiTake> {
        let st = &self.status;
        let (started, ended) = (st.rec_sessions.load(Ordering::Acquire), st.rec_ended.load(Ordering::Acquire));
        let taken = self.shared.rec_midi_taken.load(Ordering::Relaxed);
        if started != ended || ended <= taken {
            return None;
        }
        self.shared.rec_midi_taken.store(ended, Ordering::Relaxed);
        let end = f64::from_bits(st.rec_end_tick.load(Ordering::Relaxed));
        let (ls, le) = (st.loop_start.load(Ordering::Relaxed), st.loop_end.load(Ordering::Relaxed));
        let hi = if ls >= 0 { le as f64 } else { f64::NAN };
        let notes = lock(&self.shared.rec_midi).take_all(end, st.rec_pass.load(Ordering::Relaxed), hi);
        Some(MidiTake {
            notes,
            start_tick: f64::from_bits(st.rec_start_tick.load(Ordering::Relaxed)).round() as Tick,
            end_tick: end.round() as Tick,
            passes: st.rec_pass.load(Ordering::Relaxed) + 1,
        })
    }

    /// The WAV files that finished since the last call, one per armed audio
    /// track of each take: path, track and the tick of the first frame. The
    /// files are written by a background thread a moment after the take
    /// ends: poll until `recording_info().takes_finished` has caught up and
    /// the files you expect are in.
    pub fn finish_audio_recording(&self) -> Vec<AudioTake> {
        std::mem::take(&mut *lock(&self.shared.recorder.results))
    }

    /// Folder the audio takes are written to (default: a folder under the
    /// system temp directory).
    pub fn set_recording_dir(&self, dir: impl Into<std::path::PathBuf>) {
        self.shared.recorder.set_dir(dir.into());
    }

    /// Pick the device channels an audio track records and monitors
    /// (`None` = the engine's default input).
    pub fn set_track_input(&self, track: TrackId, input: Option<ChannelSelection>) {
        let sel = input.map(|c| (c.first, c.count.clamp(1, 2)));
        let _ = self.tx.send(Msg::TrackInput(track, sel));
        self.send_rt(RtCommand::SetTrackInput(track, sel.map_or(0, |s| s.0), sel.map_or(0, |s| s.1)));
    }

    /// Say that an audio track has no input at all (`true`): it is not
    /// recorded (no WAV, not in `recording_info().audio_tracks`) and not
    /// monitored, whatever its armed state, channels or monitor mode.
    /// `false` gives it its input back (the channels from
    /// [`set_track_input`](Self::set_track_input), or the default).
    pub fn set_track_input_none(&self, track: TrackId, none: bool) {
        let _ = self.tx.send(Msg::TrackNoInput(track, none));
        self.send_rt(RtCommand::SetTrackNoInput(track, none));
    }

    /// Set an audio track's monitor mode (In: always; Auto: unless the
    /// transport plays back without recording; Off: never; all only while
    /// the track is armed). `None` goes back to the project's mode.
    pub fn set_track_monitor(&self, track: TrackId, mode: Option<Monitor>) {
        let _ = self.tx.send(Msg::TrackMonitor(track, mode));
        if let Some(m) = mode {
            self.send_rt(RtCommand::SetTrackMonitor(track, m));
        }
    }

    /// Headless engines (no audio device) only: queue interleaved stereo
    /// input frames, as a device's input callback would. Returns how many
    /// samples were accepted (0 with a real device).
    pub fn feed_input(&self, interleaved: &[f32]) -> usize {
        self.feed_input_at(interleaved, self.engine_nanos())
    }

    /// Like [`feed_input`](Self::feed_input) for frames whose first one was
    /// captured at `capture_nanos` on the engine clock
    /// ([`engine_nanos`](Self::engine_nanos)): input that waited, or came
    /// from a microphone with latency. Only whole frames are queued.
    pub fn feed_input_at(&self, interleaved: &[f32], capture_nanos: u64) -> usize {
        let mut guard = lock(&self.shared.virtual_input);
        let Some(sink) = guard.as_mut() else { return 0 };
        sink.push_with(interleaved.len() / 2, 2, capture_nanos, |i| interleaved[i]) * 2
    }

    /// Fast path for a knob: set a device parameter (normalised 0..=1) on
    /// the running graph at once, without compiling. Returns `false` if the
    /// command queue is full. Does not touch the project: the caller still
    /// applies its `Command` and hands the project over (which then costs no
    /// compile either, see [`update_project`](Self::update_project)).
    pub fn set_device_param(&self, device: DeviceId, param: u32, normalized: f32) -> bool {
        let _ = self.tx.send(Msg::Patch(crate::compile::Patch::Param { device, param, value: normalized }));
        self.send_rt(RtCommand::SetParam(device, param, normalized))
    }

    /// Fast path for a fader: a track's volume in dB.
    pub fn set_track_volume(&self, track: TrackId, db: f32) -> bool {
        let _ = self.tx.send(Msg::Patch(crate::compile::Patch::Volume { track, db }));
        self.send_rt(RtCommand::SetTrackVolume(track, db))
    }

    pub fn set_track_pan(&self, track: TrackId, pan: f32) -> bool {
        let _ = self.tx.send(Msg::Patch(crate::compile::Patch::Pan { track, pan }));
        self.send_rt(RtCommand::SetTrackPan(track, pan))
    }

    /// Fast path for a send knob: the level in dB of send `index` of the
    /// track (its position in `Track::sends`).
    pub fn set_send_level(&self, track: TrackId, index: usize, db: f32) -> bool {
        let index = index.min(u16::MAX as usize) as u16;
        let _ = self.tx.send(Msg::Patch(crate::compile::Patch::Send { track, index, db }));
        self.send_rt(RtCommand::SetSend(track, index, db))
    }

    /// Fast path for a macro knob: move macro `index` of the rack device
    /// `rack` to `value` (0..=1). Every parameter mapped to it follows
    /// (`min + (max - min) * value`) on the running graph at once, and no
    /// graph is compiled. Returns `false` if the engine is gone. The
    /// project's `Macro::value` still needs its `Command`; handing the project
    /// over afterwards costs no compile either.
    pub fn set_macro(&self, rack: DeviceId, index: usize, value: f32) -> bool {
        self.tx.send(Msg::Macro(rack, index.min(u16::MAX as usize) as u16, value)).is_ok()
    }

    /// Fast path for a rack's Chain Select: the selector value (0..=127) the
    /// chains' zones are read at. No compile; the chains' gains glide to
    /// their new zone gains within the block. The project's value still
    /// needs its `Command`.
    pub fn set_rack_selector(&self, rack: DeviceId, value: u8) -> bool {
        let value = value.min(127);
        let _ = self.tx.send(Msg::Patch(crate::compile::Patch::Selector { rack, value }));
        self.send_rt(RtCommand::SetRackSelector(rack, value))
    }

    /// Fast path for the global groove amount (0..=1.3): every clip with a
    /// groove follows from a little ahead of the playhead on, and no graph is
    /// compiled. The project's value still needs its `Command`.
    pub fn set_groove_amount(&self, amount: f32) -> bool {
        let amount = if amount.is_finite() { amount.clamp(0.0, aitrack_model::MAX_GROOVE_AMOUNT) } else { 1.0 };
        let _ = self.tx.send(Msg::Patch(crate::compile::Patch::GrooveAmount(amount)));
        self.send_rt(RtCommand::SetGrooveAmount(amount))
    }

    /// Fast path for a pool groove's Quantize / Timing / Random / Velocity.
    pub fn set_groove_settings(&self, id: aitrack_model::GrooveId, settings: aitrack_model::GrooveSettings) -> bool {
        let settings = settings.normalized();
        let _ = self.tx.send(Msg::Patch(crate::compile::Patch::GrooveSettings { id, settings }));
        self.send_rt(RtCommand::SetGrooveSettings(id, settings))
    }

    /// Graphs compiled since the engine started. A drag of a knob or fader
    /// must not move this.
    pub fn compile_count(&self) -> u64 {
        self.shared.compile_count.load(Ordering::Acquire)
    }

    /// Project snapshots absorbed without a compile because only
    /// parameters, volumes, pans, send levels or display-only fields
    /// differed from the running graph.
    pub fn patch_count(&self) -> u64 {
        self.shared.patch_count.load(Ordering::Acquire)
    }

    /// Inject live MIDI (on-screen keyboard, tests), stamped with the
    /// current time. Real MIDI ports bypass this and feed the audio thread
    /// directly, stamped when the driver delivers each message.
    pub fn send_midi(&self, msg: MidiMessage) -> bool {
        self.send_midi_at(msg, self.engine_nanos().max(1))
    }

    /// Like [`send_midi`](Self::send_midi) for a message that happened at
    /// `nanos` on the engine clock ([`engine_nanos`](Self::engine_nanos)).
    /// The audio thread plays it one block after the block it arrived in,
    /// at the sample offset its time corresponds to, so messages keep their
    /// spacing; a time in the past plays at the start of the next block.
    pub fn send_midi_at(&self, msg: MidiMessage, nanos: u64) -> bool {
        lock(&self.shared.virtual_midi).as_mut().is_some_and(|p| p.push(TimedMidi { msg, nanos }).is_ok())
    }

    /// The engine clock live MIDI is stamped with: nanoseconds since the
    /// engine started.
    pub fn engine_nanos(&self) -> u64 {
        self.status.now_nanos()
    }

    pub fn send_midi_to(&self, track: TrackId, msg: MidiMessage) -> bool {
        self.send_rt(RtCommand::SendMidiTo(track, msg))
    }

    pub fn is_playing(&self) -> bool {
        self.status.playing.load(Ordering::Relaxed)
    }

    /// Audible playhead position in ticks (compensated for graph latency).
    pub fn position_ticks(&self) -> f64 {
        self.status.position_ticks()
    }

    /// Peak (since last read) and RMS of a track's post-fader output.
    pub fn meters(&self, track: TrackId) -> Option<MeterReading> {
        lock(&self.status.meters).slot(track).map(|s| s.take())
    }

    /// Meter of the final output bus.
    pub fn master_meter(&self) -> MeterReading {
        lock(&self.status.meters).master.take()
    }

    /// A reader on the analysis ring: stereo frames of the Main output (or
    /// of one track, see [`TapReader::set_source`]) for spectrum, loudness
    /// and scope displays. The ring is written only while a reader has
    /// enabled it, and never blocks the audio thread.
    pub fn analysis_tap(&self) -> TapReader {
        TapReader::new(self.status.tap.clone())
    }

    /// The latest `Processor::meter()` values of a device (spectrum, gain
    /// reduction… see each stock plugin's meter layout), copied to `out`.
    /// Returns the number of values; `0` for unknown devices and plugins
    /// without display data. Reading keeps the device publishing for a
    /// short while, so unwatched devices cost nothing.
    pub fn device_meter(&self, device: DeviceId, out: &mut [f32]) -> usize {
        let slot: Option<Arc<DeviceMeter>> = {
            let bank = lock(&self.status.device_meters);
            bank.binary_search_by_key(&device, |(id, _)| *id).ok().map(|i| bank[i].1.clone())
        };
        slot.map_or(0, |s| s.read(out))
    }

    /// `(smoothed, peak since last read)` callback time / buffer time.
    pub fn cpu_load(&self) -> (f32, f32) {
        self.status.cpu()
    }

    pub fn xruns(&self) -> u64 {
        self.status.xruns.load(Ordering::Relaxed)
    }

    /// Launch state of every Session slot holding a clip.
    pub fn clip_states(&self) -> Vec<ClipSlotStatus> {
        lock(&self.status.session).snapshot()
    }

    /// Total plugin delay compensation of the current graph, in samples.
    pub fn latency(&self) -> u32 {
        self.status.latency.load(Ordering::Relaxed)
    }

    /// Chain latency per track (for `Track::reported_latency`).
    pub fn track_latencies(&self) -> Vec<(TrackId, u32)> {
        lock(&self.status.track_latency).clone()
    }

    pub fn sample_rate(&self) -> u32 {
        self.status.sample_rate.load(Ordering::Relaxed)
    }

    pub fn set_sample_cache_bytes(&self, bytes: usize) {
        self.samples.set_budget_bytes(bytes);
    }

    pub fn sample_cache_stats(&self) -> (usize, usize) {
        (self.samples.used_bytes(), self.samples.budget_bytes())
    }

    /// Last compile or device error. A compile that fails (only a project
    /// without a Main track does) leaves the previous graph running and sets
    /// this; the next successful compile clears it.
    pub fn last_error(&self) -> Option<String> {
        lock(&self.shared.last_error).clone()
    }

    /// Non-fatal problems of the last compile (missing plugins, bad sends,
    /// an invalid output sent to Main instead, a routing cycle cut open…).
    /// Replaced by every compile, so an empty list means the project is
    /// being played as written.
    pub fn warnings(&self) -> Vec<String> {
        lock(&self.shared.warnings).clone()
    }

    /// Changes whenever a compile finishes and [`warnings`](Self::warnings) /
    /// [`last_error`](Self::last_error) may have changed: poll this and
    /// re-read them only when it moves.
    pub fn compile_generation(&self) -> u64 {
        self.shared.compile_generation.load(Ordering::Acquire)
    }

    /// Panics the audio path contained and non-finite samples it repaired
    /// since the engine started, and the devices a panic silenced.
    pub fn audio_health(&self) -> AudioHealth {
        self.status.health()
    }

    /// Audio hosts available on this machine.
    pub fn hosts(&self) -> Vec<String> {
        audio_io::hosts()
    }

    /// Audio devices of every host.
    pub fn devices(&self) -> Vec<AudioDeviceInfo> {
        audio_io::devices()
    }

    /// The device that is open, with any fallbacks applied.
    pub fn active_audio(&self) -> Option<ActiveAudio> {
        lock(&self.shared.active_audio).clone()
    }

    /// The configuration last requested (to persist in preferences).
    pub fn audio_config(&self) -> Option<AudioConfig> {
        lock(&self.shared.config).clone()
    }

    /// Switch device / rate / buffer size (or `None` to close audio). Plugin
    /// instances are kept and re-prepared; the transport position is kept.
    /// Blocks until the device is open; UIs use
    /// [`set_device_async`](Self::set_device_async).
    pub fn set_device(&self, config: Option<AudioConfig>) -> Result<Option<ActiveAudio>, EngineError> {
        self.set_device_async(config).wait(Duration::from_secs(20))
    }

    /// Like [`set_device`](Self::set_device) without waiting: the device is
    /// reopened on the control thread and the request reports when done.
    pub fn set_device_async(&self, config: Option<AudioConfig>) -> DeviceRequest {
        let (tx, rx) = bounded(1);
        if self.tx.send(Msg::SetAudio(config, tx.clone())).is_err() {
            let _ = tx.send(Err(EngineError::Disconnected));
        }
        DeviceRequest { rx }
    }

    /// Changes whenever the device is opened, reopened (lost, rerouted,
    /// came back) or closed: re-read [`active_audio`](Self::active_audio).
    pub fn audio_generation(&self) -> u64 {
        self.shared.generation.load(Ordering::Acquire)
    }

    /// Buffer, device, input and plugin latency as measured right now.
    pub fn latency_report(&self) -> LatencyReport {
        self.status.io.snapshot(self.sample_rate(), self.latency())
    }

    /// The audio thread's last playhead anchor (see [`PlayheadAnchor`]).
    pub fn playhead_anchor(&self) -> PlayheadAnchor {
        self.status.anchor.read()
    }

    /// The song position reaching the speakers now, in ticks: the audible
    /// (PDC-compensated) position interpolated between callbacks and moved
    /// back by the measured output latency (buffer + device + Bluetooth).
    /// This is what a playhead should draw.
    pub fn heard_position_ticks(&self) -> f64 {
        let a = self.status.anchor.read();
        if !a.playing {
            return self.position_ticks();
        }
        a.heard_at(self.status.now_nanos(), self.latency_report().output_seconds())
    }

    pub fn midi_inputs(&self) -> Vec<MidiPortInfo> {
        midi_io::midi_inputs()
    }

    pub fn open_midi_input(&self, port_id: &str) -> Result<(), EngineError> {
        let (tx, rx) = bounded(1);
        self.tx.send(Msg::OpenMidi(port_id.to_string(), tx)).map_err(|_| EngineError::Disconnected)?;
        rx.recv_timeout(Duration::from_secs(10)).map_err(|_| EngineError::Disconnected)?
    }

    pub fn close_midi_input(&self, port_id: &str) {
        let _ = self.tx.send(Msg::CloseMidi(port_id.to_string()));
    }
}

/// A device switch in flight ([`EngineHandle::set_device_async`]).
pub struct DeviceRequest {
    rx: Receiver<Result<Option<ActiveAudio>, EngineError>>,
}

impl DeviceRequest {
    /// The result once the control thread is done; `None` while it works.
    pub fn poll(&self) -> Option<Result<Option<ActiveAudio>, EngineError>> {
        match self.rx.try_recv() {
            Ok(r) => Some(r),
            Err(crossbeam_channel::TryRecvError::Empty) => None,
            Err(crossbeam_channel::TryRecvError::Disconnected) => Some(Err(EngineError::Disconnected)),
        }
    }

    pub fn wait(self, timeout: Duration) -> Result<Option<ActiveAudio>, EngineError> {
        self.rx.recv_timeout(timeout).map_err(|_| EngineError::Disconnected)?
    }
}

/// Why the control thread should rebuild the audio stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Reopen {
    /// The device vanished or its stream became invalid.
    Lost,
    /// Audio is wanted but no stream is open (an earlier open failed).
    Retry,
    /// The system default output changed under a default-following stream.
    Rerouted,
    /// The device the user picked is back after a fallback.
    WantedBack,
}

/// When to rebuild the stream after device events. Pure timing logic, so
/// the state machine is tested without hardware.
///
/// * Lost / no stream: reopen at once, then at most once per [`RETRY`].
/// * Rerouted: wait until the reroutes have been quiet for [`SETTLE`]
///   (connecting Bluetooth headphones switches the default two or three
///   times); the OS keeps the old stream playing meanwhile.
/// * Running on a fallback: look for the wanted device every [`PROBE`].
#[derive(Debug, Default)]
pub(crate) struct DeviceWatch {
    last_attempt: Option<Instant>,
    reroute_since: Option<Instant>,
    last_probe: Option<Instant>,
}

pub(crate) const RETRY: Duration = Duration::from_secs(1);
pub(crate) const SETTLE: Duration = Duration::from_millis(400);
pub(crate) const PROBE: Duration = Duration::from_secs(2);

/// What the control thread saw this tick.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct DeviceEvents {
    pub want_audio: bool,
    pub have_stream: bool,
    pub lost: bool,
    pub rerouted: bool,
    pub follows_default: bool,
    pub wanted_missing: bool,
}

impl DeviceWatch {
    /// Decide this tick; `wanted_present` is asked only when a probe is due.
    pub fn poll(&mut self, now: Instant, ev: DeviceEvents, wanted_present: impl FnOnce() -> bool) -> Option<Reopen> {
        if !ev.want_audio {
            *self = Self::default();
            return None;
        }
        if ev.lost || !ev.have_stream {
            if self.last_attempt.is_some_and(|t| now.duration_since(t) < RETRY) {
                return None;
            }
            self.last_attempt = Some(now);
            self.reroute_since = None;
            return Some(if ev.lost { Reopen::Lost } else { Reopen::Retry });
        }
        if ev.rerouted && ev.follows_default {
            self.reroute_since = Some(now);
        }
        if let Some(t) = self.reroute_since
            && now.duration_since(t) >= SETTLE {
                self.reroute_since = None;
                self.last_attempt = Some(now);
                return Some(Reopen::Rerouted);
            }
        if ev.wanted_missing && self.reroute_since.is_none() {
            if self.last_probe.is_some_and(|t| now.duration_since(t) < PROBE) {
                return None;
            }
            self.last_probe = Some(now);
            if wanted_present() {
                self.last_attempt = Some(now);
                return Some(Reopen::WantedBack);
            }
        }
        None
    }

    /// The device was opened by the user (Preferences): start over.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// A reopen succeeded: the next failure may retry at once.
    pub fn opened(&mut self) {
        self.last_attempt = None;
    }
}

impl Drop for EngineHandle {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

// ───────────────────────────── Control thread ─────────────────────────────

struct Control {
    factory: Arc<dyn ProcessorFactory>,
    pool: ProcessorPool,
    samples: SampleCache,
    status: Arc<SharedStatus>,
    shared: Arc<HandleShared>,
    project: Option<Arc<Project>>,
    sample_rate: f64,
    max_frames: usize,
    graph_tx: Option<Producer<Box<CompiledGraph>>>,
    retire_rx: Option<Consumer<Box<CompiledGraph>>>,
    pending: Option<Box<CompiledGraph>>,
    graveyard: Sender<Box<CompiledGraph>>,
    stream: Option<OpenStream>,
    audio: Option<AudioConfig>,
    midi_ports: Vec<String>,
    midi_conns: Vec<MidiConnection>,
    midi_spare: Vec<Producer<TimedMidi>>,
    /// Compiles so far, and the generation of the newest graph.
    compile_gen: u64,
    /// The project the running graph was compiled from (updated by patches).
    compiled: Option<Arc<Project>>,
    /// Per-track input / monitor choices made through the API, reapplied to every graph.
    track_io: std::collections::HashMap<TrackId, TrackIo>,
    /// Latency each processor had when the current graph was compiled.
    assumed_latency: Vec<(Arc<SharedProcessor>, u32)>,
    dirty: bool,
    count_in: u32,
    watch: DeviceWatch,
    /// Callback panics seen so far, and when the latest one was noticed: a
    /// graph is rebuilt shortly after, so the halted callback can restart.
    seen_callback_panics: u64,
    panic_seen_at: Option<Instant>,
}

impl Control {
    fn run(mut self, rx: Receiver<Msg>) {
        let mut last_latency_check = Instant::now();
        // Opens the device right away when `open_in_background` left it to us.
        self.check_device();
        loop {
            match rx.recv_timeout(Duration::from_millis(10)) {
                Ok(msg) => {
                    if !self.handle(msg) {
                        break;
                    }
                    // Coalesce bursts of edits into one compile.
                    while let Ok(msg) = rx.try_recv() {
                        if !self.handle(msg) {
                            self.shutdown();
                            return;
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            self.collect_retired();
            self.flush_pending();
            if self.dirty {
                self.recompile();
            }
            if last_latency_check.elapsed() > Duration::from_millis(100) {
                last_latency_check = Instant::now();
                if self.assumed_latency.iter().any(|(p, l)| p.reported_latency() != *l) {
                    self.dirty = true;
                }
                self.check_device();
                self.retry_after_callback_panic();
            }
        }
        self.shutdown();
    }

    /// A callback that panicked stays silent until a new graph is queued:
    /// build one (half a second after the panic, so a deterministic fault
    /// retries at most twice a second).
    fn retry_after_callback_panic(&mut self) {
        let panics = self.status.callback_panics.load(Ordering::Relaxed);
        if panics != self.seen_callback_panics {
            self.seen_callback_panics = panics;
            self.panic_seen_at = Some(Instant::now());
        }
        if self.panic_seen_at.is_some_and(|t| t.elapsed() >= Duration::from_millis(500)) {
            self.panic_seen_at = None;
            self.dirty = true;
        }
    }

    fn shutdown(&mut self) {
        self.stream = None;
        self.midi_conns.clear();
        self.collect_retired();
        self.pending = None;
    }

    /// Returns `false` on shutdown.
    fn handle(&mut self, msg: Msg) -> bool {
        match msg {
            Msg::Project(p) => {
                if self.try_patch(&p) {
                    self.project = Some(p.clone());
                    self.compiled = Some(p);
                } else {
                    self.project = Some(p);
                    self.dirty = true;
                }
            }
            Msg::Recompile => self.dirty = true,
            Msg::Flush(reply) => {
                self.collect_retired();
                self.flush_pending();
                if self.dirty {
                    self.recompile();
                }
                let _ = reply.send(());
            }
            Msg::SetAudio(cfg, reply) => {
                *lock(&self.shared.config) = cfg.clone();
                self.audio = cfg.clone();
                self.watch.reset();
                let res = match cfg {
                    Some(c) => self.open_audio(c).map(Some),
                    None => {
                        self.close_audio();
                        Ok(None)
                    }
                };
                let _ = reply.send(res);
            }
            Msg::OpenMidi(id, reply) => {
                let res = self.open_midi(&id);
                if res.is_ok() && !self.midi_ports.contains(&id) {
                    self.midi_ports.push(id);
                }
                let _ = reply.send(res);
            }
            Msg::CloseMidi(id) => {
                self.midi_ports.retain(|p| *p != id);
                if let Some(i) = self.midi_conns.iter().position(|c| c.id == id) {
                    // The queue comes back, so a port can be toggled forever.
                    self.midi_spare.push(self.midi_conns.remove(i).close());
                }
                self.publish_open_midi();
            }
            Msg::TrackInput(track, input) => {
                self.track_io.entry(track).or_default().input = input;
                // A graph compiled before this message must not outlive it.
                self.dirty = true;
            }
            Msg::TrackMonitor(track, monitor) => {
                self.track_io.entry(track).or_default().monitor = monitor;
                // (None: back to the project's mode, which only a rebuild knows.)
                self.dirty = true;
            }
            #[cfg(test)]
            Msg::TestRebuild(reply) => {
                let rate = self.sample_rate;
                let runner = self.headless_runner(rate);
                let _ = reply.send(runner);
            }
            Msg::Patch(patch) => {
                // Keep the copies recompiles start from in step with the fast
                // path, or the next compile would undo the drag.
                if let Some(p) = self.project.as_mut() {
                    crate::compile::apply_patch(Arc::make_mut(p), &patch);
                }
                if let Some(c) = self.compiled.as_mut()
                    && self.project.as_ref().is_none_or(|p| !Arc::ptr_eq(p, c))
                {
                    crate::compile::apply_patch(Arc::make_mut(c), &patch);
                }
                if let crate::compile::Patch::Param { device, param, value } = patch {
                    self.pool.note_applied(device, param, value);
                }
                // The command may have reached a graph about to be replaced:
                // build the next one from the patched project.
                if self.status.graph_gen.load(Ordering::Acquire) != self.compile_gen {
                    self.dirty = true;
                }
            }
            Msg::Macro(rack, index, value) => self.move_macro(rack, index, value),
            Msg::TrackNoInput(track, none) => {
                self.track_io.entry(track).or_default().none = none;
                // (The list of tracks that would record changes with it.)
                self.dirty = true;
            }
            Msg::Shutdown => return false,
        }
        true
    }

    /// Apply `new` to the running graph as parameter updates instead of
    /// compiling, when that is all that changed (see
    /// [`crate::compile::param_patches`]). `false` = compile.
    fn try_patch(&mut self, new: &Arc<Project>) -> bool {
        // A graph not yet installed (or an install waiting on a full retire
        // queue) would miss the updates: compile.
        // The newest graph must be the one the audio thread runs: a patch
        // sent while a newer graph is still on its way would reach the old
        // one, and the new one would install the values it was compiled with.
        let installed = self.pending.is_none()
            && self.retire_rx.as_ref().is_some_and(|r| r.slots() < GRAPH_QUEUE / 2)
            && self.status.graph_gen.load(Ordering::Acquire) == self.compile_gen;
        if self.dirty || !installed {
            return false;
        }
        let Some(old) = self.compiled.clone() else { return false };
        let Some(patches) = crate::compile::param_patches(&old, new) else { return false };
        let mut commands = lock(&self.shared.commands);
        let Some(tx) = commands.as_mut() else { return false };
        if tx.slots() < patches.len() {
            return false;
        }
        for patch in patches {
            use crate::compile::Patch;
            let cmd = match patch {
                Patch::Param { device, param, value } => {
                    self.pool.note_applied(device, param, value);
                    RtCommand::SetParam(device, param, value)
                }
                Patch::Volume { track, db } => RtCommand::SetTrackVolume(track, db),
                Patch::Pan { track, pan } => RtCommand::SetTrackPan(track, pan),
                Patch::Send { track, index, db } => RtCommand::SetSend(track, index, db),
                Patch::MacroValue { .. } => continue,
                Patch::Selector { rack, value } => RtCommand::SetRackSelector(rack, value),
                Patch::GrooveSettings { id, settings } => RtCommand::SetGrooveSettings(id, settings),
                Patch::GrooveAmount(amount) => RtCommand::SetGrooveAmount(amount),
                Patch::ModRange { device, index, min, max } => RtCommand::SetModRange(device, index, min, max),
            };
            let _ = tx.push(cmd);
        }
        self.shared.patch_count.fetch_add(1, Ordering::Release);
        true
    }

    /// Move a macro on the running graph: every mapped parameter gets
    /// `min + (max - min) * value` as a parameter command, and the control
    /// thread's project copies keep the macro's value so a recompile starts
    /// from it. No compile.
    fn move_macro(&mut self, rack: DeviceId, index: u16, value: f32) {
        let value = value.clamp(0.0, 1.0);
        let mappings = self
            .compiled
            .as_deref()
            .or(self.project.as_deref())
            .and_then(|p| p.tracks.iter().find_map(|t| crate::compile::find_rack(&t.devices, rack)))
            .and_then(|r| r.macros.get(index as usize))
            .map(|m| m.mappings.clone())
            .unwrap_or_default();
        let patch = crate::compile::Patch::MacroValue { rack, index, value };
        if let Some(p) = self.project.as_mut() {
            crate::compile::apply_patch(Arc::make_mut(p), &patch);
        }
        if let Some(c) = self.compiled.as_mut()
            && self.project.as_ref().is_none_or(|p| !Arc::ptr_eq(p, c))
        {
            crate::compile::apply_patch(Arc::make_mut(c), &patch);
        }
        {
            let mut commands = lock(&self.shared.commands);
            if let Some(tx) = commands.as_mut() {
                for m in crate::compile::macro_patches(&mappings, value) {
                    if let crate::compile::Patch::Param { device, param, value } = m {
                        self.pool.note_applied(device, param, value);
                        let _ = tx.push(RtCommand::SetParam(device, param, value));
                    }
                }
            }
        }
        if self.status.graph_gen.load(Ordering::Acquire) != self.compile_gen {
            self.dirty = true;
        }
    }

    fn publish_open_midi(&self) {
        *lock(&self.shared.midi_open) = self.midi_conns.iter().map(|c| c.id.clone()).collect();
    }

    fn collect_retired(&mut self) {
        if let Some(rx) = &mut self.retire_rx {
            while let Ok(g) = rx.pop() {
                let _ = self.graveyard.send(g);
            }
        }
    }

    fn flush_pending(&mut self) {
        if let (Some(g), Some(tx)) = (self.pending.take(), self.graph_tx.as_mut())
            && let Err(rtrb::PushError::Full(g)) = tx.push(g) {
                self.pending = Some(g);
            }
    }

    fn recompile(&mut self) {
        // Never compile past an uninstalled graph: its parameter diffs would
        // be lost if it were replaced.
        if self.pending.is_some() || self.graph_tx.as_ref().is_some_and(|t| t.slots() == 0) {
            return;
        }
        self.dirty = false;
        let Some(project) = self.project.clone() else { return };
        let prev_meters = Some(lock(&self.status.meters).clone());
        let mut cx = CompileContext {
            factory: &*self.factory,
            pool: &mut self.pool,
            samples: &self.samples,
            sample_rate: self.sample_rate,
            max_frames: self.max_frames,
            prev_meters,
            taps: Vec::new(),
            analysis: Some(self.status.tap.clone()),
        };
        self.shared.compile_count.fetch_add(1, Ordering::Release);
        match compile(&project, &mut cx) {
            Ok(mut g) => {
                self.compiled = Some(project.clone());
                self.compile_gen += 1;
                g.generation = self.compile_gen;
                for t in &mut g.tracks {
                    if let Some(io) = self.track_io.get(&t.id) {
                        t.input_sel = io.input;
                        t.no_input = io.none;
                        if let Some(m) = io.monitor {
                            t.monitor = m;
                        }
                    }
                }
                *lock(&self.status.armed_midi) = g.tracks.iter().filter(|t| t.arm && t.kind == aitrack_model::TrackKind::Instrument).map(|t| t.id).collect();
                *lock(&self.status.armed_audio) = g.tracks.iter().filter(|t| t.arm && t.kind == aitrack_model::TrackKind::Audio && !t.no_input).map(|t| t.id).collect();
                *lock(&self.status.meters) = g.meters.clone();
                let mut device_meters = Vec::new();
                for t in &g.tracks {
                    crate::graph::for_each_proc(&t.devices, &mut |p| device_meters.push((p.id, p.meter.clone())));
                }
                device_meters.sort_by_key(|(id, _)| *id);
                *lock(&self.status.device_meters) = device_meters;
                let mut crash_flags = Vec::new();
                for t in &g.tracks {
                    crate::graph::for_each_proc(&t.devices, &mut |p| crash_flags.push((p.id, p.proc.crash_flag())));
                }
                crash_flags.sort_by_key(|(id, _)| *id);
                *lock(&self.status.crash_flags) = crash_flags;
                *lock(&self.status.session) = g.session_status.clone();
                *lock(&self.status.track_latency) = g.track_latency.clone();
                *lock(&self.shared.warnings) = g.warnings.clone();
                *lock(&self.shared.last_error) = None;
                self.shared.compile_generation.fetch_add(1, Ordering::Release);
                self.assumed_latency.clear();
                for t in &g.tracks {
                    collect_latency(&t.devices, &mut self.assumed_latency);
                }
                let g = Box::new(g);
                match self.graph_tx.as_mut() {
                    Some(tx) => {
                        if let Err(rtrb::PushError::Full(g)) = tx.push(g) {
                            self.pending = Some(g);
                        }
                    }
                    None => drop(g),
                }
            }
            Err(e) => {
                log::warn!("graph compile failed: {e}");
                *lock(&self.shared.last_error) = Some(e.to_string());
                self.shared.compile_generation.fetch_add(1, Ordering::Release);
            }
        }
    }

    /// Fresh queues and a new [`AudioProcessor`]; the handle's command and
    /// virtual-MIDI producers and every MIDI port are re-wired to it.
    fn new_runner(&mut self, sample_rate: f64, input: Option<InputLink>) -> AudioProcessor {
        let (graph_tx, graph_rx) = RingBuffer::new(GRAPH_QUEUE);
        let (retire_tx, retire_rx) = RingBuffer::new(GRAPH_QUEUE);
        let (cmd_tx, cmd_rx) = RingBuffer::new(COMMAND_QUEUE);
        let mut midi_rx = Vec::with_capacity(MIDI_QUEUES);
        let mut midi_tx = Vec::with_capacity(MIDI_QUEUES);
        for _ in 0..MIDI_QUEUES {
            let (t, r) = RingBuffer::new(MIDI_QUEUE);
            midi_tx.push(t);
            midi_rx.push(r);
        }
        self.collect_retired();
        self.graph_tx = Some(graph_tx);
        self.retire_rx = Some(retire_rx);
        self.pending = None;
        *lock(&self.shared.commands) = Some(cmd_tx);
        *lock(&self.shared.virtual_midi) = midi_tx.pop();
        self.midi_spare = midi_tx;
        self.midi_conns.clear();
        for id in self.midi_ports.clone() {
            if let Err(e) = self.open_midi(&id) {
                log::warn!("MIDI input {id}: {e}");
            }
        }
        self.publish_open_midi();
        let loop_start = self.status.loop_start.load(Ordering::Relaxed);
        let init = TransportInit {
            playing: self.status.playing.load(Ordering::Relaxed),
            position_ticks: self.status.position_ticks(),
            loop_range: (loop_start >= 0).then(|| (loop_start, self.status.loop_end.load(Ordering::Relaxed))),
            metronome: self.status.metronome.load(Ordering::Relaxed),
            count_in_bars: self.count_in,
        };
        self.sample_rate = sample_rate;
        let q = RtQueues {
            graph_rx,
            retire_tx,
            cmd_rx,
            midi_rx,
            input_rx: input.map(|(rx, stamps, channels, first, count)| InputQueue { rx, stamps, channels, first, count }),
        };
        let mut runner = AudioProcessor::new(q, self.status.clone(), sample_rate, self.max_frames, init);
        let (rec_midi_tx, rec_midi_rx) = RingBuffer::new(REC_MIDI_QUEUE);
        let (rec_audio_tx, rec_audio_rx) = RingBuffer::new(AUDIO_QUEUE);
        runner.attach_recorder(rec_midi_tx, rec_audio_tx);
        // The old audio thread is gone. A take it was recording ends here, so
        // the app gets what was captured and the next take can start: the
        // notes still in the old queue are collected, the files are closed.
        let st = &self.status;
        let mid_take = st.rec_sessions.load(Ordering::Acquire) != st.rec_ended.load(Ordering::Acquire);
        {
            let mut notes = lock(&self.shared.rec_midi);
            notes.drain();
            notes.rx = Some(rec_midi_rx);
        }
        self.shared.recorder.attach(rec_audio_rx);
        if mid_take {
            st.rec_end_tick.store(st.position_ticks().to_bits(), Ordering::Relaxed);
            st.rec_capturing.store(false, Ordering::Release);
            st.rec_ended.store(st.rec_sessions.load(Ordering::Acquire), Ordering::Release);
            self.shared.recorder.close_open();
        }
        // Compile right away so the first callback already has a graph.
        self.dirty = true;
        self.recompile();
        runner
    }

    /// A runner with no device: a stereo input queue the caller feeds by hand.
    fn headless_runner(&mut self, rate: f64) -> AudioProcessor {
        let (sink, rx, stamps) = input_ring(rate as usize, 2);
        *lock(&self.shared.virtual_input) = Some(sink);
        self.new_runner(rate, Some((rx, stamps, 2, 0, 2)))
    }

    fn open_midi(&mut self, id: &str) -> Result<(), EngineError> {
        if self.midi_conns.iter().any(|c| c.id == id) {
            return Ok(());
        }
        let conn = midi_io::connect(id, &mut self.midi_spare, self.status.epoch)?;
        self.midi_conns.push(conn);
        self.publish_open_midi();
        Ok(())
    }

    fn close_audio(&mut self) {
        self.stream = None;
        *lock(&self.shared.active_audio) = None;
        self.status.io.reset();
        self.shared.generation.fetch_add(1, Ordering::AcqRel);
    }

    fn open_audio(&mut self, cfg: AudioConfig) -> Result<ActiveAudio, EngineError> {
        // Tear the old stream down first: after this no graph is running,
        // so pooled processors may be re-prepared on this thread.
        if self.stream.take().is_some() {
            *lock(&self.shared.active_audio) = None;
            self.shared.generation.fetch_add(1, Ordering::AcqRel);
        }
        let prepared = audio_io::prepare(&cfg)?;
        let sr = prepared.sample_rate as f64;
        if (sr - self.sample_rate).abs() > 0.5 {
            // SAFETY: the stream (and the processor that ran the graphs)
            // was dropped above; headless processors handed out earlier
            // must no longer be driven (documented on `set_device`).
            unsafe { self.pool.reprepare_all(sr, self.max_frames) };
            self.samples.clear();
        }
        self.pool.forget_applied();
        let (input_tx, input_rx) = if prepared.active.input_channels > 0 {
            let ch = prepared.active.input_channels as usize;
            let (sink, r, stamps) = input_ring(sr as usize, ch);
            (Some(sink), Some((r, stamps, ch, prepared.in_first, prepared.in_count)))
        } else {
            (None, None)
        };
        let runner = self.new_runner(sr, input_rx);
        let stream = audio_io::start(prepared, runner, input_tx, self.status.clone())?;
        let active = stream.active.clone();
        for n in &active.notes {
            log::info!("audio: {n}");
        }
        *lock(&self.shared.active_audio) = Some(active.clone());
        self.stream = Some(stream);
        self.shared.generation.fetch_add(1, Ordering::AcqRel);
        Ok(active)
    }

    /// Rebuild the stream when the device was lost (unplugged, driver
    /// reset, sample rate changed under it), the system default output
    /// moved (headphones, Bluetooth), an earlier open failed, or the device
    /// the user picked came back. Runs on the control thread, never the UI's.
    fn check_device(&mut self) {
        let stream = self.stream.as_ref();
        let active = stream.map(|s| &s.active);
        let ev = DeviceEvents {
            want_audio: self.audio.is_some(),
            have_stream: stream.is_some(),
            lost: stream.is_some_and(|s| s.lost.load(Ordering::Acquire)),
            rerouted: stream.is_some_and(|s| s.rerouted.swap(false, Ordering::AcqRel)),
            follows_default: active.is_some_and(|a| a.follows_default),
            wanted_missing: active.is_some_and(|a| a.wanted_missing),
        };
        let cfg = self.audio.clone();
        let Some(reason) = self.watch.poll(Instant::now(), ev, || cfg.as_ref().is_some_and(audio_io::output_present)) else { return };
        let Some(cfg) = cfg else { return };
        match reason {
            Reopen::Lost => log::warn!("audio device lost; reopening"),
            Reopen::Rerouted => log::info!("default output changed; reopening on the new device"),
            Reopen::WantedBack => log::info!("audio device is back; switching to it"),
            Reopen::Retry => {}
        }
        if reason == Reopen::Lost {
            self.stream = None;
            *lock(&self.shared.active_audio) = None;
            self.shared.generation.fetch_add(1, Ordering::AcqRel);
        }
        match self.open_audio(cfg.clone()).or_else(|_| {
            // Fall back to the default device of the same host.
            self.open_audio(AudioConfig { output_device: None, output_device_name: None, input_device: None, input_device_name: None, ..cfg })
        }) {
            // The persisted config keeps the user's preferred device.
            Ok(_) => {
                self.watch.opened();
                *lock(&self.shared.last_error) = None;
            }
            Err(e) => *lock(&self.shared.last_error) = Some(e.to_string()),
        }
    }
}

fn collect_latency(devs: &[crate::graph::DeviceNode], out: &mut Vec<(Arc<SharedProcessor>, u32)>) {
    for d in devs {
        match d {
            crate::graph::DeviceNode::Proc(p) => out.push((p.proc.clone(), p.proc.reported_latency())),
            crate::graph::DeviceNode::Rack(r) => {
                for c in r.chains.iter().chain(&r.returns) {
                    collect_latency(&c.devices, out);
                }
            }
            crate::graph::DeviceNode::Missing(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev() -> DeviceEvents {
        DeviceEvents { want_audio: true, have_stream: true, ..DeviceEvents::default() }
    }

    #[test]
    fn healthy_stream_does_nothing() {
        let mut w = DeviceWatch::default();
        let t = Instant::now();
        assert_eq!(w.poll(t, ev(), || panic!("no probe")), None);
        assert_eq!(w.poll(t, DeviceEvents::default(), || panic!("no probe")), None, "audio off");
    }

    #[test]
    fn lost_device_reopens_at_once_then_backs_off() {
        let mut w = DeviceWatch::default();
        let t = Instant::now();
        let lost = DeviceEvents { lost: true, ..ev() };
        assert_eq!(w.poll(t, lost, || false), Some(Reopen::Lost));
        // The reopen failed: no stream now; wait RETRY before trying again.
        let none = DeviceEvents { have_stream: false, ..ev() };
        assert_eq!(w.poll(t + Duration::from_millis(500), none, || false), None);
        assert_eq!(w.poll(t + RETRY, none, || false), Some(Reopen::Retry));
        // Success clears the back-off.
        w.opened();
        assert_eq!(w.poll(t + RETRY + Duration::from_millis(10), lost, || false), Some(Reopen::Lost));
    }

    #[test]
    fn reroutes_settle_before_one_rebuild() {
        let mut w = DeviceWatch::default();
        let t = Instant::now();
        let follow = DeviceEvents { follows_default: true, ..ev() };
        let reroute = DeviceEvents { rerouted: true, ..follow };
        assert_eq!(w.poll(t, reroute, || false), None);
        // A second switch 200 ms later restarts the wait.
        assert_eq!(w.poll(t + Duration::from_millis(200), reroute, || false), None);
        assert_eq!(w.poll(t + Duration::from_millis(500), follow, || false), None);
        assert_eq!(w.poll(t + Duration::from_millis(200) + SETTLE, follow, || false), Some(Reopen::Rerouted));
        assert_eq!(w.poll(t + Duration::from_secs(5), follow, || false), None, "once");
    }

    #[test]
    fn reroute_of_a_pinned_device_is_ignored() {
        let mut w = DeviceWatch::default();
        let t = Instant::now();
        let pinned = DeviceEvents { rerouted: true, ..ev() };
        assert_eq!(w.poll(t, pinned, || false), None);
        assert_eq!(w.poll(t + SETTLE * 2, ev(), || false), None);
    }

    #[test]
    fn wanted_device_is_probed_and_reclaimed() {
        let mut w = DeviceWatch::default();
        let t = Instant::now();
        let fallback = DeviceEvents { wanted_missing: true, follows_default: true, ..ev() };
        assert_eq!(w.poll(t, fallback, || false), None);
        // Not probed again before PROBE.
        assert_eq!(w.poll(t + Duration::from_millis(100), fallback, || panic!("too soon")), None);
        assert_eq!(w.poll(t + PROBE, fallback, || true), Some(Reopen::WantedBack));
    }

    /// Needs an output device: `cargo test -p aitrack-engine -- --ignored real_device`.
    #[test]
    #[ignore]
    fn real_device_opens_in_background_and_reports_latency() {
        let cfg = EngineConfig { audio: Some(AudioConfig::default()), open_in_background: true, ..EngineConfig::default() };
        let started = Instant::now();
        let (h, _) = Engine::new(cfg, Arc::new(crate::NullFactory)).unwrap();
        assert!(started.elapsed() < Duration::from_millis(50), "Engine::new did not wait for the device");
        h.update_project(&aitrack_model::Project::new("t"));
        let deadline = Instant::now() + Duration::from_secs(5);
        while (h.active_audio().is_none() || h.latency_report().output_frames == 0) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let a = h.active_audio().expect("device opened");
        let r = h.latency_report();
        eprintln!("opened {} after {:?}: {a:?}\n{r:?}\nbuffer {:.2} ms, output {:.2} ms (device {:.2} ms)", a.output_device, started.elapsed(), r.buffer_ms(), r.output_ms(), r.device_output_ms());
        assert_eq!(r.buffer_frames, a.buffer_size.unwrap_or(r.buffer_frames));
        assert!(r.output_frames >= r.buffer_frames);
        assert!(h.audio_generation() >= 1);
        h.play();
        std::thread::sleep(Duration::from_millis(300));
        let engine = h.position_ticks();
        let heard = h.heard_position_ticks();
        eprintln!("engine {engine:.1} ticks, heard {heard:.1} ticks");
        assert!(heard <= engine + 1.);
        // Switching buffer size reopens off the caller's thread.
        let req = h.set_device_async(Some(AudioConfig { buffer_size: Some(128), ..AudioConfig::default() }));
        let a2 = req.wait(Duration::from_secs(10)).unwrap().unwrap();
        eprintln!("reopened at buffer {:?}", a2.buffer_size);
    }

    #[test]
    fn async_device_switch_reports_disconnect_without_a_device() {
        // Headless engine: closing audio answers on the control thread.
        let (h, _p) = Engine::new(EngineConfig::headless(48_000.), Arc::new(crate::NullFactory)).unwrap();
        let g = h.audio_generation();
        let req = h.set_device_async(None);
        let r = req.wait(Duration::from_secs(5)).expect("closed");
        assert!(r.is_none());
        assert!(h.audio_generation() > g, "closing bumps the generation");
        assert_eq!(h.latency_report().output_frames, 0);
        assert!(!h.playhead_anchor().playing);
    }
}
