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

use crate::audio_io::{self, ActiveAudio, AudioConfig, AudioDeviceInfo, OpenStream};
use crate::compile::{compile, CompileContext};
use crate::factory::ProcessorFactory;
use crate::graph::CompiledGraph;
use crate::midi_io::{self, MidiConnection, MidiPortInfo};
use crate::rt::{AudioProcessor, InputQueue, MidiMessage, RtCommand, RtQueues, TransportInit};
use crate::sample_cache::SampleCache;
use crate::shared::{ProcessorPool, SharedProcessor};
use crate::status::{ClipSlotStatus, LatencyReport, MeterReading, PlayheadAnchor, SharedStatus};
use crate::tap::{DeviceMeter, TapReader};
use crate::EngineError;
use aitrack_model::{DeviceId, Project, Quantize, SceneId, Tick, TrackId};
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
    Shutdown,
}

/// State shared between the handle and the control thread.
struct HandleShared {
    commands: Mutex<Option<Producer<RtCommand>>>,
    virtual_midi: Mutex<Option<Producer<MidiMessage>>>,
    last_error: Mutex<Option<String>>,
    warnings: Mutex<Vec<String>>,
    active_audio: Mutex<Option<ActiveAudio>>,
    config: Mutex<Option<AudioConfig>>,
    /// Bumped every time the device is opened, reopened or closed.
    generation: AtomicU64,
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
            assumed_latency: Vec::new(),
            dirty: false,
            count_in: config.count_in_bars,
            watch: DeviceWatch::default(),
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
            None => Some(control.new_runner(config.headless_sample_rate, None)),
        };
        let thread = std::thread::Builder::new()
            .name("aitrack-engine-control".into())
            .spawn(move || control.run(rx))
            .map_err(EngineError::Io)?;
        Ok((
            EngineHandle { tx, shared, status, thread: Some(thread), _graveyard: Some(graveyard) },
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
        self.send_rt(RtCommand::SetMidiTarget(track));
    }

    /// Inject live MIDI (on-screen keyboard, tests). Real MIDI ports bypass
    /// this and feed the audio thread directly.
    pub fn send_midi(&self, msg: MidiMessage) -> bool {
        lock(&self.shared.virtual_midi).as_mut().is_some_and(|p| p.push(msg).is_ok())
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

    /// Last compile or device error.
    pub fn last_error(&self) -> Option<String> {
        lock(&self.shared.last_error).clone()
    }

    /// Non-fatal problems of the last compile (missing plugins, bad sends…).
    pub fn warnings(&self) -> Vec<String> {
        lock(&self.shared.warnings).clone()
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
    midi_spare: Vec<Producer<MidiMessage>>,
    /// Latency each processor had when the current graph was compiled.
    assumed_latency: Vec<(Arc<SharedProcessor>, u32)>,
    dirty: bool,
    count_in: u32,
    watch: DeviceWatch,
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
            }
        }
        self.shutdown();
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
                self.project = Some(p);
                self.dirty = true;
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
                self.midi_conns.retain(|c| c.id != id);
            }
            Msg::Shutdown => return false,
        }
        true
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
        match compile(&project, &mut cx) {
            Ok(g) => {
                *lock(&self.status.meters) = g.meters.clone();
                let mut device_meters = Vec::new();
                for t in &g.tracks {
                    crate::graph::for_each_proc(&t.devices, &mut |p| device_meters.push((p.id, p.meter.clone())));
                }
                device_meters.sort_by_key(|(id, _)| *id);
                *lock(&self.status.device_meters) = device_meters;
                *lock(&self.status.session) = g.session_status.clone();
                *lock(&self.status.track_latency) = g.track_latency.clone();
                *lock(&self.shared.warnings) = g.warnings.clone();
                *lock(&self.shared.last_error) = None;
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
            }
        }
    }

    /// Fresh queues and a new [`AudioProcessor`]; the handle's command and
    /// virtual-MIDI producers and every MIDI port are re-wired to it.
    fn new_runner(&mut self, sample_rate: f64, input: Option<(Consumer<f32>, usize, usize, usize)>) -> AudioProcessor {
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
            input_rx: input.map(|(rx, channels, first, count)| InputQueue { rx, channels, first, count }),
        };
        let runner = AudioProcessor::new(q, self.status.clone(), sample_rate, self.max_frames, init);
        // Compile right away so the first callback already has a graph.
        self.dirty = true;
        self.recompile();
        runner
    }

    fn open_midi(&mut self, id: &str) -> Result<(), EngineError> {
        if self.midi_conns.iter().any(|c| c.id == id) {
            return Ok(());
        }
        let producer = self.midi_spare.pop().ok_or_else(|| EngineError::Midi("too many MIDI inputs open".into()))?;
        let conn = midi_io::connect(id, producer)?;
        self.midi_conns.push(conn);
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
            let (t, r) = RingBuffer::new((sr as usize / 2).max(4096) * ch);
            (Some(t), Some((r, ch, prepared.in_first, prepared.in_count)))
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
                for c in &r.chains {
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
