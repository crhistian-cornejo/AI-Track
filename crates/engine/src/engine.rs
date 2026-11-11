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
//!   reopens the device when it is lost, recompiles when plugin latency
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
use crate::status::{ClipSlotStatus, MeterReading, SharedStatus};
use crate::EngineError;
use aitrack_model::{Project, Quantize, SceneId, Tick, TrackId};
use crossbeam_channel::{bounded, unbounded, Receiver, RecvTimeoutError, Sender};
use rtrb::{Consumer, Producer, RingBuffer};
use std::sync::atomic::Ordering;
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
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self { audio: Some(AudioConfig::default()), headless_sample_rate: 48_000.0, max_block: 1024, midi_inputs: Vec::new(), count_in_bars: 0 }
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
            last_reopen: None,
        };
        let manual = match &config.audio {
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
    pub fn set_device(&self, config: Option<AudioConfig>) -> Result<Option<ActiveAudio>, EngineError> {
        let (tx, rx) = bounded(1);
        self.tx.send(Msg::SetAudio(config, tx)).map_err(|_| EngineError::Disconnected)?;
        rx.recv_timeout(Duration::from_secs(20)).map_err(|_| EngineError::Disconnected)?
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
    last_reopen: Option<Instant>,
}

impl Control {
    fn run(mut self, rx: Receiver<Msg>) {
        let mut last_latency_check = Instant::now();
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
                self.last_reopen = None;
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
        };
        match compile(&project, &mut cx) {
            Ok(g) => {
                *lock(&self.status.meters) = g.meters.clone();
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
    }

    fn open_audio(&mut self, cfg: AudioConfig) -> Result<ActiveAudio, EngineError> {
        // Tear the old stream down first: after this no graph is running,
        // so pooled processors may be re-prepared on this thread.
        self.stream = None;
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
        Ok(active)
    }

    /// Reopen the device after it was lost (unplugged, driver reset).
    fn check_device(&mut self) {
        let lost = self.stream.as_ref().is_some_and(|s| s.lost.load(Ordering::Acquire));
        let want_audio = self.audio.is_some() && self.stream.is_none();
        if !lost && !want_audio {
            return;
        }
        if lost {
            log::warn!("audio device lost; reopening");
            self.stream = None;
            *lock(&self.shared.active_audio) = None;
        }
        if self.last_reopen.is_some_and(|t| t.elapsed() < Duration::from_secs(1)) {
            return;
        }
        self.last_reopen = Some(Instant::now());
        let Some(cfg) = self.audio.clone() else { return };
        match self.open_audio(cfg.clone()).or_else(|_| {
            // Fall back to the default device of the same host.
            self.open_audio(AudioConfig { output_device: None, output_device_name: None, input_device: None, input_device_name: None, ..cfg })
        }) {
            // The persisted config keeps the user's preferred device.
            Ok(_) => self.last_reopen = None,
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
