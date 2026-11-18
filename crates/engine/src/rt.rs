//! The audio-thread side of the engine.
//!
//! [`AudioProcessor`] is what the device callback (or a test, or the
//! offline renderer) calls once per buffer. Per block it:
//! 1. installs a newly compiled graph if one is waiting *and* the retire
//!    queue has room (the old graph goes back to be dropped elsewhere);
//! 2. applies transport / launch commands and live MIDI;
//! 3. splits the block at loop wraps and count-in end, and processes each
//!    segment through the graph (on the DSP worker pool, when it has more
//!    than this thread; see [`crate::multicore`]);
//! 4. adds the metronome and publishes meters, position, clip states, CPU.
//!
//! Nothing here allocates, locks or does I/O.

use crate::clip::TimeCtx;
use crate::graph::{guarded, CompiledGraph, SegmentIo};
use crate::record::{RecChunk, RecEvent, RecKind, RecState};
use crate::status::{PlayheadAnchor, SharedStatus};
use crate::workers::WorkerPool;
use aitrack_dsp::{AudioBuffer, EventKind};
use aitrack_model::{DeviceId, Monitor, Quantize, SceneId, TempoMap, Tick, TrackId, TrackKind, PPQ};
use rtrb::{Consumer, Producer};
use std::sync::atomic::{AtomicU32, AtomicU8, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

// ───────────────────────────── Panic hook ─────────────────────────────

thread_local! {
    /// Raised while a device callback runs: a panic here is answered with
    /// silence by the guard, and the hook must not print or allocate.
    static IN_AUDIO: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Mark the calling thread (a DSP worker) as one whose panics are answered
/// by a guard: the hook stays silent there for good.
pub(crate) fn mark_in_audio() {
    IN_AUDIO.with(|f| f.set(true));
}

type PanicHook = dyn Fn(&std::panic::PanicHookInfo<'_>) + Sync + Send + 'static;

const NOTE_BYTES: usize = 128;

/// Where the last panic on the audio thread happened (`file:line`), written
/// by the hook without allocating and read by the UI. Diagnostic only: a
/// read that races a write is dropped.
struct PanicNote {
    seq: AtomicU32,
    len: AtomicUsize,
    line: AtomicU32,
    file: [AtomicU8; NOTE_BYTES],
}

static LAST_AUDIO_PANIC: PanicNote = PanicNote { seq: AtomicU32::new(0), len: AtomicUsize::new(0), line: AtomicU32::new(0), file: [const { AtomicU8::new(0) }; NOTE_BYTES] };

/// `file:line` of the most recent panic the audio thread contained, if any.
pub(crate) fn last_audio_panic() -> Option<String> {
    let n = &LAST_AUDIO_PANIC;
    let s0 = n.seq.load(Ordering::Acquire);
    if s0 == 0 || s0 & 1 == 1 {
        return None;
    }
    let len = n.len.load(Ordering::Relaxed).min(NOTE_BYTES);
    let bytes: Vec<u8> = n.file[..len].iter().map(|b| b.load(Ordering::Relaxed)).collect();
    let line = n.line.load(Ordering::Relaxed);
    (n.seq.load(Ordering::Acquire) == s0).then(|| format!("{}:{line}", String::from_utf8_lossy(&bytes)))
}

/// A hook that, on the audio thread, only records where the panic was (no
/// printing, no allocation, no locks) and, anywhere else, runs `prev`.
pub(crate) fn audio_aware_hook(prev: Box<PanicHook>) -> Box<PanicHook> {
    Box::new(move |info| {
        if !IN_AUDIO.with(std::cell::Cell::get) {
            prev(info);
            return;
        }
        let n = &LAST_AUDIO_PANIC;
        let s = n.seq.load(Ordering::Relaxed);
        n.seq.store(s.wrapping_add(1) | 1, Ordering::Release);
        if let Some(loc) = info.location() {
            let f = loc.file().as_bytes();
            let f = &f[f.len().saturating_sub(NOTE_BYTES)..];
            for (slot, b) in n.file.iter().zip(f) {
                slot.store(*b, Ordering::Relaxed);
            }
            n.len.store(f.len(), Ordering::Relaxed);
            n.line.store(loc.line(), Ordering::Relaxed);
        }
        n.seq.store((s | 1).wrapping_add(1), Ordering::Release);
    })
}

/// Install [`audio_aware_hook`] over the current hook, once per process.
/// Called when a processor is built (control thread), never from a callback.
fn install_panic_hook() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let prev = std::panic::take_hook();
        std::panic::set_hook(audio_aware_hook(prev));
    });
}

/// Commands from the control side to the audio thread.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum RtCommand {
    Play,
    Stop,
    Pause,
    Seek(Tick),
    SetLoop(Option<(Tick, Tick)>),
    SetMetronome(bool),
    SetCountIn(u32),
    SetRecording(bool),
    LaunchClip(TrackId, SceneId),
    LaunchScene(SceneId),
    StopTrack(TrackId),
    StopAllClips,
    BackToArrangement,
    SetMidiTarget(Option<TrackId>),
    SendMidiTo(TrackId, MidiMessage),
    SetGlobalQuantize(Quantize),
    /// Fast path for a knob: a device parameter (normalised), applied to the
    /// installed graph without compiling.
    SetParam(DeviceId, u32, f32),
    /// The range of a modulator's mapping: `(modulator, mapping index, min, max)`.
    SetModRange(DeviceId, u16, f32, f32),
    /// Fast path for a fader: volume in dB.
    SetTrackVolume(TrackId, f32),
    SetTrackPan(TrackId, f32),
    /// Fast path for a send knob: `(track, index in the track's send list, dB)`.
    SetSend(TrackId, u16, f32),
    /// An audio track's input channels `(first, count)`; count 0 = the engine's default input.
    SetTrackInput(TrackId, u16, u16),
    SetTrackMonitor(TrackId, Monitor),
    /// The track has no input at all: it neither records nor monitors.
    SetTrackNoInput(TrackId, bool),
    /// A rack's Chain Select value.
    SetRackSelector(DeviceId, u8),
    /// The global groove amount.
    SetGrooveAmount(f32),
    /// A pool groove's settings.
    SetGrooveSettings(aitrack_model::GrooveId, aitrack_model::GrooveSettings),
}

/// A raw short MIDI message (status + up to two data bytes).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MidiMessage {
    pub bytes: [u8; 3],
    pub len: u8,
}

impl MidiMessage {
    pub fn from_slice(data: &[u8]) -> Option<Self> {
        if data.is_empty() || data.len() > 3 || data[0] < 0x80 {
            return None;
        }
        let mut bytes = [0u8; 3];
        bytes[..data.len()].copy_from_slice(data);
        Some(Self { bytes, len: data.len() as u8 })
    }

    pub fn note_on(channel: u8, key: u8, velocity: u8) -> Self {
        Self { bytes: [0x90 | (channel & 15), key & 127, velocity & 127], len: 3 }
    }

    pub fn note_off(channel: u8, key: u8) -> Self {
        Self { bytes: [0x80 | (channel & 15), key & 127, 0], len: 3 }
    }

    /// Translate to an engine event.
    pub fn to_event(self) -> Option<EventKind> {
        let [status, d1, d2] = self.bytes;
        let channel = status & 0x0F;
        let v = d2 as f32 / 127.0;
        Some(match status & 0xF0 {
            0x90 if d2 > 0 => EventKind::NoteOn { channel, key: d1, velocity: v, note_id: -1 },
            0x80 | 0x90 => EventKind::NoteOff { channel, key: d1, velocity: v, note_id: -1 },
            0xB0 => EventKind::ControlChange { channel, cc: d1, value: v },
            0xE0 => EventKind::PitchBend { channel, value: (((d2 as i32) << 7 | d1 as i32) - 8192) as f32 / 8192.0 },
            0xD0 => EventKind::ChannelPressure { channel, value: d1 as f32 / 127.0 },
            _ => return None,
        })
    }
}

/// A live event waiting for its chunk: `off` frames into the callback span.
#[derive(Clone, Copy)]
struct PendingMidi {
    off: u32,
    ev: EventKind,
}

/// A live MIDI message with the engine-clock time it happened at
/// ([`SharedStatus::epoch`] nanoseconds; `0` = unknown, played at once).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct TimedMidi {
    pub msg: MidiMessage,
    pub nanos: u64,
}

/// Fractional tick → samples on a tempo map (no allocation).
pub(crate) fn tick_f_to_samples(tempo: &TempoMap, tick: f64, sr: f64) -> f64 {
    let ti = tick.floor() as Tick;
    let frac = tick - ti as f64;
    (tempo.tick_to_seconds(ti) + frac * 60.0 / (tempo.bpm_at(ti) * PPQ as f64)) * sr
}

/// Synthesised metronome click: a decaying sine burst.
struct Click {
    phase: f32,
    inc: f32,
    env: f32,
    decay: f32,
    /// Pending triggers in the current segment: `(offset, accent)`.
    triggers: [(usize, bool); 8],
    n_triggers: usize,
    level: f32,
}

impl Click {
    fn new(sr: f64) -> Self {
        Self {
            phase: 0.0,
            inc: 0.0,
            env: 0.0,
            decay: (-1.0 / (0.025 * sr)).exp() as f32,
            triggers: [(0, false); 8],
            n_triggers: 0,
            level: 0.5,
        }
    }

    fn trigger(&mut self, offset: usize, accent: bool) {
        if self.n_triggers < self.triggers.len() {
            self.triggers[self.n_triggers] = (offset, accent);
            self.n_triggers += 1;
        }
    }

    fn render(&mut self, out: &mut AudioBuffer, from: usize, to: usize, sr: f64) {
        let mut ti = 0;
        let (l, r) = out.stereo_mut();
        for k in from..to {
            while ti < self.n_triggers && self.triggers[ti].0 + from <= k {
                let accent = self.triggers[ti].1;
                self.inc = (if accent { 1760.0 } else { 1320.0 } / sr) as f32;
                self.phase = 0.0;
                self.env = if accent { 1.0 } else { 0.7 };
                ti += 1;
            }
            if self.env > 1e-4 {
                let s = (self.phase * std::f32::consts::TAU).sin() * self.env * self.level;
                l[k] += s;
                r[k] += s;
                self.phase = (self.phase + self.inc).fract();
                self.env *= self.decay;
            }
        }
        self.n_triggers = 0;
    }
}

pub(crate) struct RtQueues {
    pub graph_rx: Consumer<Box<CompiledGraph>>,
    pub retire_tx: Producer<Box<CompiledGraph>>,
    pub cmd_rx: Consumer<RtCommand>,
    pub midi_rx: Vec<Consumer<TimedMidi>>,
    /// Interleaved capture samples.
    pub input_rx: Option<InputQueue>,
}

/// When a device input callback captured its first frame: `nanos` on the
/// engine clock ([`SharedStatus::epoch`]), `first` = frames pushed before it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct InputStamp {
    pub first: u64,
    pub nanos: u64,
}

/// The input side of the device input queue: the samples and, per callback,
/// when they were captured. Whole frames only.
pub(crate) struct InputSink {
    pub samples: Producer<f32>,
    pub stamps: Producer<InputStamp>,
    pub written: u64,
}

impl InputSink {
    /// Push up to `frames` frames of `channels` samples (`sample(i)` is
    /// sample `i` of the callback), captured from `capture_nanos`. Returns
    /// the frames that fit; the rest are lost and the caller counts them.
    pub fn push_with(&mut self, frames: usize, channels: usize, capture_nanos: u64, sample: impl Fn(usize) -> f32) -> usize {
        let ch = channels.max(1);
        let n = frames.min(self.samples.slots() / ch);
        if n > 0 {
            let _ = self.stamps.push(InputStamp { first: self.written, nanos: capture_nanos });
            for i in 0..n * ch {
                let _ = self.samples.push(sample(i));
            }
            self.written += n as u64;
        }
        n
    }
}

pub(crate) struct InputQueue {
    pub rx: Consumer<f32>,
    pub stamps: Consumer<InputStamp>,
    /// Channels per interleaved frame.
    pub channels: usize,
    /// Selected device channels feeding the engine input (mono or stereo).
    pub first: usize,
    pub count: usize,
}

/// Transport state handed over when the processor is rebuilt (device change).
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct TransportInit {
    pub playing: bool,
    pub position_ticks: f64,
    pub loop_range: Option<(Tick, Tick)>,
    pub metronome: bool,
    pub count_in_bars: u32,
}

/// The real-time engine. Owned by the audio callback (or driven manually in
/// headless mode via [`process_interleaved`](Self::process_interleaved) /
/// [`process_planar`](Self::process_planar)).
pub struct AudioProcessor {
    graph: Option<Box<CompiledGraph>>,
    q: RtQueues,
    status: Arc<SharedStatus>,
    sample_rate: f64,
    max_frames: usize,
    playing: bool,
    recording: bool,
    pos: i64,
    play_start: i64,
    /// Position to adopt (in ticks) once the first graph is installed.
    pending_ticks: Option<f64>,
    loop_range: Option<(Tick, Tick)>,
    metronome: bool,
    count_in_bars: u32,
    count_in_total: i64,
    count_in_left: i64,
    midi_target: Option<TrackId>,
    global_quantize: Quantize,
    click: Click,
    out: AudioBuffer,
    input: AudioBuffer,
    thread_promoted: bool,
    /// Start of the device callback being processed ([`SharedStatus::epoch`]
    /// clock) and how many of its frames came before the current block.
    cb_nanos: u64,
    cb_offset: usize,
    run: RunTracker,
    /// Test hook: make the next block panic (see `process_interleaved_guarded`).
    #[cfg(test)]
    pub(crate) fail_blocks: u32,
    /// Recording (see [`crate::record`]).
    rec: RecState,
    /// The block's device input, interleaved (`raw_ch` channels per frame).
    raw_in: Vec<f32>,
    raw_ch: usize,
    /// Test hook run between a block's graph install and its commands.
    #[cfg(test)]
    pub(crate) hook_after_install: Option<Box<dyn FnMut() + Send>>,
    /// Input frames taken from the queue so far (dropped ones included).
    in_read: u64,
    /// The newest input stamp at or before `in_read`.
    cur_stamp: Option<InputStamp>,
    /// Engine-clock time the first frame of the current block was captured at.
    in_block_nanos: Option<i64>,
    /// Live MIDI read from the queues, waiting for the chunk of the callback it falls in.
    midi_pending: Vec<PendingMidi>,
    /// Frames of the current read already handed out (the chunk base of `midi_pending`).
    midi_base: usize,
    /// Frames of the device callback being processed (`0` outside one).
    cb_total: usize,
    /// Engine position at the end of the last block: what live MIDI stamps are measured from.
    last_end_pos: i64,
    /// A callback panicked: stay silent, without entering the block again,
    /// until a new graph arrives.
    halted: bool,
    /// The DSP threads that help this one (none: the single-threaded path).
    pool: WorkerPool,
    /// A replacement pool (the "Audio threads" setting changed) and the way
    /// back for the one it replaces, dropped (joined) off this thread.
    pool_rx: Option<Consumer<WorkerPool>>,
    pool_retire: Option<Producer<WorkerPool>>,
}

/// Continuous-run bookkeeping for the heard playhead, in engine samples:
/// where the run started, where the previous run ended (loop wrap), and
/// where the engine expects to be next if nothing jumps.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct RunTracker {
    pub start: i64,
    pub prev_end: Option<i64>,
    pub expected: Option<i64>,
}

impl RunTracker {
    /// A playing segment starts at `pos`: a jump (play start, seek, tempo
    /// remap, loop wrap) begins a new run. Real-time safe.
    #[inline]
    pub fn note(&mut self, pos: i64, loop_samples: Option<(i64, i64)>) {
        if self.expected == Some(pos) {
            return;
        }
        let wrapped = matches!((self.expected, loop_samples), (Some(e), Some((ls, le))) if e == le && pos == ls);
        self.prev_end = if wrapped { self.expected } else { None };
        self.start = pos;
    }

    /// The segment advanced the engine to `pos`.
    #[inline]
    pub fn advanced(&mut self, pos: i64) {
        self.expected = Some(pos);
    }

    /// Not playing: the next playing segment starts a fresh run.
    #[inline]
    pub fn stopped(&mut self) {
        self.expected = None;
    }
}

impl AudioProcessor {
    pub(crate) fn new(q: RtQueues, status: Arc<SharedStatus>, sample_rate: f64, max_frames: usize, init: TransportInit) -> Self {
        status.sample_rate.store(sample_rate as u32, Ordering::Relaxed);
        install_panic_hook();
        let raw_ch = q.input_rx.as_ref().map_or(0, |i| i.channels.max(1));
        Self {
            graph: None,
            q,
            status,
            sample_rate,
            max_frames,
            playing: init.playing,
            recording: false,
            pos: 0,
            play_start: 0,
            pending_ticks: Some(init.position_ticks),
            loop_range: init.loop_range,
            metronome: init.metronome,
            count_in_bars: init.count_in_bars,
            count_in_total: 0,
            count_in_left: 0,
            midi_target: None,
            global_quantize: Quantize::Bar,
            click: Click::new(sample_rate),
            out: AudioBuffer::new(2, max_frames),
            input: AudioBuffer::new(2, max_frames),
            thread_promoted: false,
            cb_nanos: 0,
            cb_offset: 0,
            run: RunTracker::default(),
            #[cfg(test)]
            fail_blocks: 0,
            halted: false,
            rec: RecState::new(max_frames, sample_rate as u32),
            raw_in: vec![0.0; max_frames * raw_ch],
            raw_ch,
            last_end_pos: 0,
            #[cfg(test)]
            hook_after_install: None,
            in_read: 0,
            cur_stamp: None,
            in_block_nanos: None,
            midi_pending: Vec::with_capacity(4096),
            midi_base: 0,
            cb_total: 0,
            pool: WorkerPool::serial(),
            pool_rx: None,
            pool_retire: None,
        }
    }

    /// Run the graph on `pool` from now on (control thread, before the
    /// processor goes to the audio thread, or an offline renderer).
    pub(crate) fn set_pool(&mut self, pool: WorkerPool) {
        self.pool = pool;
        self.status.audio_threads.store(self.pool.threads() as u32, Ordering::Relaxed);
    }

    /// Wire the queues a running processor gets new pools through.
    pub(crate) fn attach_pools(&mut self, rx: Consumer<WorkerPool>, retire: Producer<WorkerPool>) {
        self.pool_rx = Some(rx);
        self.pool_retire = Some(retire);
    }

    /// Threads the graph runs on (this one included).
    pub fn audio_threads(&self) -> usize {
        self.pool.threads()
    }

    /// The installed pool, for the allocation counter of the tests.
    #[cfg(test)]
    pub(crate) fn pool_id(&self) -> usize {
        self.pool.id()
    }

    /// Swap in a pool waiting in the queue, when the old one can go back.
    /// Real-time safe: two queue operations, the old pool is joined elsewhere.
    fn install_pool(&mut self) {
        let (Some(rx), Some(retire)) = (self.pool_rx.as_mut(), self.pool_retire.as_mut()) else { return };
        while retire.slots() > 0 {
            let Ok(new) = rx.pop() else { break };
            let old = std::mem::replace(&mut self.pool, new);
            if let Err(rtrb::PushError::Full(old)) = retire.push(old) {
                // Cannot happen (slots were checked); never drop here.
                std::mem::forget(old);
            }
            self.status.audio_threads.store(self.pool.threads() as u32, Ordering::Relaxed);
        }
    }

    /// Mark the start of a device callback (for the heard-playhead anchor).
    /// The cpal callbacks call this; offline drivers need not.
    #[inline]
    pub fn begin_callback(&mut self) {
        self.cb_nanos = self.status.now_nanos();
        self.cb_offset = 0;
    }

    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    pub fn max_frames(&self) -> usize {
        self.max_frames
    }

    /// Start of the device callback being processed, on the engine clock
    /// (what live MIDI stamps are measured against).
    #[cfg(test)]
    pub(crate) fn callback_start_nanos(&self) -> u64 {
        self.cb_nanos
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// Engine position in samples (before PDC compensation).
    pub fn position_samples(&self) -> i64 {
        self.pos
    }

    /// Wire the recording queues (control thread, before the callback runs).
    pub(crate) fn attach_recorder(&mut self, midi_tx: rtrb::Producer<RecEvent>, audio_tx: rtrb::Producer<RecChunk>) {
        self.rec.midi_tx = Some(midi_tx);
        self.rec.audio_tx = Some(audio_tx);
    }

    /// The device channels audio tracks use when they pick none.
    fn default_input_sel(&self) -> (usize, usize) {
        self.q.input_rx.as_ref().map_or((0, 2), |i| (i.first, i.count))
    }

    /// Start or end a take when the transport state asks for it.
    fn rec_update(&mut self) {
        let want = self.recording && self.playing && self.count_in_left == 0;
        if want == self.rec.capturing {
            return;
        }
        let sel = self.default_input_sel();
        let queue = self.input_queue_frames();
        let Some(g) = self.graph.as_deref() else { return };
        if want {
            self.rec.begin(g, &self.status, self.pos, self.sample_rate, 0, sel, queue);
            self.last_end_pos = self.pos;
        } else {
            self.rec.end(g, &self.status, self.last_end_pos, self.sample_rate);
        }
    }

    /// Total graph latency in samples.
    pub fn latency(&self) -> u32 {
        self.graph.as_ref().map_or(0, |g| g.total_latency)
    }

    pub(crate) fn graph_mut(&mut self) -> Option<&mut CompiledGraph> {
        self.graph.as_deref_mut()
    }

    pub(crate) fn mark_thread_promoted(&mut self) -> bool {
        std::mem::replace(&mut self.thread_promoted, true)
    }

    /// Fill an interleaved buffer of `channels` channels. Real-time safe.
    pub fn process_interleaved(&mut self, data: &mut [f32], channels: usize) {
        self.process_interleaved_at(data, channels, 0);
    }

    /// Like [`process_interleaved`](Self::process_interleaved), writing the
    /// stereo output to device channels `first` and `first + 1` (other
    /// channels are silent). Real-time safe.
    pub fn process_interleaved_at(&mut self, data: &mut [f32], channels: usize, first: usize) {
        let channels = channels.max(1);
        let first = first.min(channels - 1);
        let stereo = first + 1 < channels;
        self.cb_total = data.len() / channels;
        for chunk in data.chunks_mut(self.max_frames * channels) {
            let frames = chunk.len() / channels;
            self.process_block(frames);
            let (l, r) = (self.out.channel(0), self.out.channel(1));
            for (f, frame) in chunk.chunks_exact_mut(channels).enumerate() {
                frame.fill(0.0);
                if stereo {
                    frame[first] = l[f];
                    frame[first + 1] = r[f];
                } else {
                    frame[first] = 0.5 * (l[f] + r[f]);
                }
            }
        }
        self.cb_total = 0;
    }

    /// [`process_interleaved_at`](Self::process_interleaved_at) for a device
    /// callback: a panic anywhere in the block is caught, the whole buffer is
    /// answered with silence and [`AudioHealth::callback_panics`] counts it.
    /// Returns `false` when it produced silence because of a panic.
    ///
    /// After a panic the engine stays silent, without entering the block
    /// again, until a new graph is waiting (the control thread builds one a
    /// moment after a panic): a deterministic fault would otherwise fail on
    /// every callback. A panic must not unwind out of a callback that C code
    /// (CoreAudio, WASAPI) called: that aborts the process. Real-time safe;
    /// free when nothing panics.
    pub fn process_interleaved_guarded(&mut self, data: &mut [f32], channels: usize, first: usize) -> bool {
        if self.halted && self.q.graph_rx.is_empty() {
            data.fill(0.0);
            return false;
        }
        IN_AUDIO.with(|f| f.set(true));
        let ran = guarded(|| self.process_interleaved_at(data, channels, first));
        IN_AUDIO.with(|f| f.set(false));
        self.halted = ran.is_none();
        if ran.is_none() {
            data.fill(0.0);
            self.status.callback_panics.fetch_add(1, Ordering::Relaxed);
        }
        ran.is_some()
    }

    /// Fill a planar buffer (`frames ≤ max_frames`). Real-time safe.
    pub fn process_planar(&mut self, out: &mut AudioBuffer) {
        let frames = out.frames().min(self.max_frames);
        self.process_block(frames);
        for ch in 0..out.channels() {
            let src = self.out.channel(ch.min(1));
            out.channel_mut(ch)[..frames].copy_from_slice(src);
        }
    }

    fn install_graphs(&mut self) {
        while self.q.retire_tx.slots() > 0 || self.graph.is_none() {
            let Ok(mut new) = self.q.graph_rx.pop() else { break };
            if let Some(mut old) = self.graph.take() {
                if old.tempo != new.tempo {
                    let tick = old.tempo.samples_to_tick(self.pos as f64, self.sample_rate);
                    let start_tick = old.tempo.samples_to_tick(self.play_start as f64, self.sample_rate);
                    self.pos = tick_f_to_samples(&new.tempo, tick, self.sample_rate).round() as i64;
                    self.play_start = tick_f_to_samples(&new.tempo, start_tick, self.sample_rate).round() as i64;
                    // Everything held as a sample position moves with the tempo,
                    // or a take in progress would compare ticks of two tempi.
                    let (old_tempo, sr) = (&old.tempo, self.sample_rate);
                    let remap = |s: i64| tick_f_to_samples(&new.tempo, old_tempo.samples_to_tick(s as f64, sr), sr).round() as i64;
                    self.rec.start_pos = remap(self.rec.start_pos);
                    self.rec.end_pos = remap(self.rec.end_pos);
                    self.last_end_pos = remap(self.last_end_pos);
                }
                new.adopt(&mut old);
                if let Err(rtrb::PushError::Full(old)) = self.q.retire_tx.push(old) {
                    // Cannot happen (slots were checked); never drop here.
                    std::mem::forget(old);
                }
            } else if let Some(t) = self.pending_ticks.take() {
                self.pos = tick_f_to_samples(&new.tempo, t, self.sample_rate).round() as i64;
                self.play_start = self.pos;
            }
            new.apply_init_params();
            self.status.latency.store(new.total_latency, Ordering::Relaxed);
            self.status.graph_gen.store(new.generation, Ordering::Release);
            self.graph = Some(new);
        }
    }

    fn now_ticks(&self) -> f64 {
        self.graph.as_ref().map_or(0.0, |g| g.tempo.samples_to_tick(self.pos as f64, self.sample_rate))
    }

    fn start_playing(&mut self) {
        if self.playing || self.count_in_left > 0 {
            return;
        }
        self.play_start = self.pos;
        if self.count_in_bars > 0
            && let Some(g) = &self.graph {
                let tick = self.now_ticks().floor() as Tick;
                let bar = g.tempo.signature_at(tick).bar_ticks() as f64;
                let bar_samples = bar / PPQ as f64 * 60.0 / g.tempo.bpm_at(tick) * self.sample_rate;
                self.count_in_total = (bar_samples * self.count_in_bars as f64).round() as i64;
                self.count_in_left = self.count_in_total;
                return;
            }
        self.playing = true;
    }

    fn apply_commands(&mut self) {
        while let Ok(cmd) = self.q.cmd_rx.pop() {
            match cmd {
                RtCommand::Play => self.start_playing(),
                RtCommand::Pause => {
                    self.playing = false;
                    self.count_in_left = 0;
                    if let Some(g) = &mut self.graph {
                        g.release_all_notes();
                    }
                }
                RtCommand::Stop => {
                    if self.playing || self.count_in_left > 0 {
                        self.pos = self.play_start;
                    } else {
                        self.pos = 0;
                        self.play_start = 0;
                    }
                    self.playing = false;
                    self.count_in_left = 0;
                    if let Some(g) = &mut self.graph {
                        g.stop_all_session();
                        g.release_all_notes();
                    }
                }
                RtCommand::Seek(tick) => {
                    if let Some(g) = &mut self.graph {
                        self.pos = g.tempo.tick_to_samples(tick, self.sample_rate).round() as i64;
                        g.release_all_notes();
                    } else {
                        self.pending_ticks = Some(tick as f64);
                    }
                    if !self.playing {
                        self.play_start = self.pos;
                    }
                }
                RtCommand::SetLoop(r) => self.loop_range = r.filter(|(a, b)| b > a),
                RtCommand::SetMetronome(on) => self.metronome = on,
                RtCommand::SetCountIn(bars) => self.count_in_bars = bars,
                RtCommand::SetRecording(on) => self.recording = on,
                RtCommand::SetMidiTarget(t) => self.midi_target = t,
                RtCommand::SendMidiTo(track, msg) => {
                    if let Some(g) = &mut self.graph
                        && let Some(t) = g.track_index(track)
                        && let Some(event) = msg.to_event()
                    {
                        g.push_live(t, 0, event);
                    }
                }
                RtCommand::SetGlobalQuantize(q) => self.global_quantize = q,
                RtCommand::SetParam(device, param, v) => {
                    if let Some(g) = &mut self.graph {
                        g.set_device_param(device, param, v);
                    }
                }
                RtCommand::SetModRange(device, index, min, max) => {
                    if let Some(g) = &mut self.graph {
                        g.set_mod_range(device, index, min, max);
                    }
                }
                RtCommand::SetTrackVolume(track, db) => {
                    if let Some(g) = &mut self.graph {
                        g.set_track_volume(track, db);
                    }
                }
                RtCommand::SetTrackPan(track, pan) => {
                    if let Some(g) = &mut self.graph {
                        g.set_track_pan(track, pan);
                    }
                }
                RtCommand::SetTrackInput(track, first, count) => {
                    if let Some(g) = &mut self.graph {
                        g.set_track_input(track, (count > 0).then_some((first, count)));
                    }
                }
                RtCommand::SetGrooveAmount(amount) => {
                    let now = self.now_ticks().ceil() as Tick;
                    if let Some(g) = &mut self.graph {
                        g.set_groove_amount(amount, now);
                    }
                }
                RtCommand::SetGrooveSettings(id, settings) => {
                    let now = self.now_ticks().ceil() as Tick;
                    if let Some(g) = &mut self.graph {
                        g.set_groove_settings(id, settings, now);
                    }
                }
                RtCommand::SetRackSelector(rack, value) => {
                    if let Some(g) = &mut self.graph {
                        g.set_rack_selector(rack, value);
                    }
                }
                RtCommand::SetTrackNoInput(track, none) => {
                    if let Some(g) = &mut self.graph {
                        g.set_track_no_input(track, none);
                    }
                }
                RtCommand::SetTrackMonitor(track, monitor) => {
                    if let Some(g) = &mut self.graph {
                        g.set_track_monitor(track, monitor);
                    }
                }
                RtCommand::SetSend(track, k, db) => {
                    if let Some(g) = &mut self.graph {
                        g.set_send_level(track, k, db);
                    }
                }
                RtCommand::LaunchClip(track, scene) => {
                    let now = self.now_ticks();
                    let immediate = !self.playing;
                    let gq = self.global_quantize;
                    if let Some(g) = &mut self.graph
                        && let (Some(t), Some(s)) = (g.track_index(track), g.scene_index(scene)) {
                            g.launch(t, s, now, immediate, gq);
                        }
                    self.start_playing();
                }
                RtCommand::LaunchScene(scene) => {
                    let now = self.now_ticks();
                    let immediate = !self.playing;
                    let gq = self.global_quantize;
                    if let Some(g) = &mut self.graph
                        && let Some(s) = g.scene_index(scene) {
                            g.launch_scene(s, now, immediate, gq);
                        }
                    self.start_playing();
                }
                RtCommand::StopTrack(track) => {
                    let now = self.now_ticks();
                    let gq = if self.playing { self.global_quantize } else { Quantize::None };
                    if let Some(g) = &mut self.graph
                        && let Some(t) = g.track_index(track) {
                            g.stop_track(t, now, gq);
                        }
                }
                RtCommand::StopAllClips => {
                    if let Some(g) = &mut self.graph {
                        g.stop_all_session();
                    }
                }
                RtCommand::BackToArrangement => {
                    if let Some(g) = &mut self.graph {
                        g.back_to_arrangement();
                    }
                }
            }
        }
    }

    /// When the block about to run starts, on the engine clock; `None` for
    /// a processor driven by hand (tests, offline renders) that never
    /// announced a callback: wall-clock stamps mean nothing there.
    fn block_start_nanos(&self) -> Option<u64> {
        (self.cb_nanos != 0).then(|| self.cb_nanos + (self.cb_offset as f64 / self.sample_rate * 1e9) as u64)
    }

    /// Hand the live MIDI that arrived since the last block to the tracks,
    /// each message at the sample offset inside this block where it
    /// happened. Messages are played one block late: the ones that arrived
    /// during the previous block's time span land in this block at
    /// `time − (block start − block length)`, so the spacing between them
    /// survives. Older ones (a stalled callback) land at offset 0.
    ///
    /// A device callback larger than the internal block is processed in
    /// several chunks: the first one reads the whole span of the previous
    /// callback and parks the messages in `midi_pending`; each chunk then
    /// takes the ones that fall in it.
    fn read_midi(&mut self, frames: usize) {
        if self.graph.is_none() {
            for rx in &mut self.q.midi_rx {
                while rx.pop().is_ok() {}
            }
            self.midi_pending.clear();
            return;
        }
        if self.midi_pending.is_empty() {
            self.refill_midi(frames);
        }
        let Some(g) = self.graph.as_deref_mut() else { return };
        let target = self.midi_target.and_then(|t| g.track_index(t));
        let (base, end) = (self.midi_base, self.midi_base + frames);
        self.midi_pending.retain(|p| {
            if (p.off as usize) >= end {
                return true;
            }
            let off = p.off.saturating_sub(base as u32);
            match target {
                Some(t) => g.push_live(t, off, p.ev),
                None => {
                    for ti in 0..g.tracks.len() {
                        let t = &g.tracks[ti];
                        if t.arm && t.kind == TrackKind::Instrument {
                            g.push_live(ti, off, p.ev);
                        }
                    }
                }
            }
            false
        });
        self.midi_base = end;
        self.rec.midi_tail = false;
    }

    /// Drain the MIDI queues into `midi_pending` with their offsets in the
    /// span about to be played, and record the notes of armed tracks.
    fn refill_midi(&mut self, frames: usize) {
        let sr = self.sample_rate;
        let span = if self.cb_offset == 0 { self.cb_total.max(frames) } else { frames };
        let window_start = self.block_start_nanos().map(|t| t as i64 - (span as f64 / sr * 1e9) as i64);
        let Some(g) = self.graph.as_deref() else { return };
        let last = span.saturating_sub(1) as i64;
        let target = self.midi_target.and_then(|t| g.track_index(t));
        // Recording: positions are measured from the end of the previous
        // block, back by the audible delay (device output + plugin delay).
        let recording = self.rec.capturing || self.rec.midi_tail;
        let out_lat = self.status.io.output.load(Ordering::Relaxed) as i64;
        let loop_s = self.loop_range.filter(|_| recording).map(|(a, b)| {
            let ctx = TimeCtx { tempo: &g.tempo, sr, seg_start: 0 };
            (ctx.sample_of(a), ctx.sample_of(b))
        });
        let lend = self.loop_range.map_or(f64::NAN, |(_, b)| b as f64);
        let rf = self.last_end_pos;
        let mut seen = 0u64;
        self.midi_base = 0;
        for rx in &mut self.q.midi_rx {
            while let Ok(TimedMidi { msg, nanos }) = rx.pop() {
                seen += 1;
                let Some(ev) = msg.to_event() else { continue };
                let raw = match window_start {
                    Some(w) if nanos != 0 => Some(((nanos as i64 - w) as f64 * sr / 1e9).round() as i64),
                    _ => None,
                };
                if self.midi_pending.len() < self.midi_pending.capacity() {
                    self.midi_pending.push(PendingMidi { off: raw.map_or(0, |r| r.clamp(0, last) as u32), ev });
                } else {
                    self.status.rec_dropped.fetch_add(1, Ordering::Relaxed);
                }
                if !recording {
                    continue;
                }
                let capture = |rec: &mut RecState, status: &SharedStatus, track: usize| {
                    let t = &g.tracks[track];
                    if !t.arm || t.kind != TrackKind::Instrument {
                        return;
                    }
                    let kind = match ev {
                        EventKind::NoteOn { channel, key, velocity, .. } => RecKind::NoteOn { channel, key, velocity },
                        EventKind::NoteOff { channel, key, .. } => RecKind::NoteOff { channel, key },
                        _ => return,
                    };
                    let mut pos = raw.map_or(rf, |r| rf + (r - span as i64) - out_lat - g.total_latency as i64);
                    let mut pass = rec.pass;
                    if let Some((ls, le)) = loop_s
                        && le > ls && pass > 0 && pos < ls && rf >= ls && rf <= le
                    {
                        pos += le - ls;
                        pass -= 1;
                    }
                    if (pass == 0 && pos < rec.start_pos) || (rec.midi_tail && pos > rec.end_pos) {
                        return;
                    }
                    let tick = g.tempo.samples_to_tick(pos as f64, sr);
                    rec.push_event(status, RecEvent { track: t.id.0, tick, pass, loop_end: lend, kind });
                };
                match target {
                    Some(t) => capture(&mut self.rec, &self.status, t),
                    None => {
                        for ti in 0..g.tracks.len() {
                            capture(&mut self.rec, &self.status, ti);
                        }
                    }
                }
            }
        }
        if seen > 0 {
            self.status.midi_messages.fetch_add(seen, Ordering::Relaxed);
        }
    }

    fn read_input(&mut self, frames: usize) -> bool {
        let Some(InputQueue { rx, stamps, channels, first, count }) = &mut self.q.input_rx else { return false };
        let ch = (*channels).max(1);
        let (first, mono) = ((*first).min(ch - 1), *count < 2 || *first + 1 >= ch);
        self.input.set_frames(frames);
        // When the first frame of this block was captured: the newest stamp
        // that starts at or before it, plus the frames since.
        while stamps.peek().is_ok_and(|s| s.first <= self.in_read) {
            self.cur_stamp = stamps.pop().ok();
        }
        let sr = self.sample_rate;
        self.in_block_nanos = self.cur_stamp.map(|s| s.nanos as i64 + ((self.in_read - s.first) as f64 / sr * 1e9) as i64);
        let take = (rx.slots() / ch).min(frames);
        if take < frames {
            self.status.input_underruns.fetch_add((frames - take) as u64, Ordering::Relaxed);
        }
        self.in_read += take as u64;
        // The whole device input of the block, kept for tracks that pick
        // their own channels and for recording.
        let raw = &mut self.raw_in[..frames * ch];
        for (i, s) in raw.iter_mut().enumerate() {
            *s = if i < take * ch { rx.pop().unwrap_or(0.0) } else { 0.0 };
        }
        let (l, r) = self.input.stereo_mut();
        for f in 0..frames {
            l[f] = raw[f * ch + first];
            r[f] = if mono { l[f] } else { raw[f * ch + first + 1] };
        }
        // Drop a backlog (whole frames) so monitoring latency cannot grow; it
        // is a gap in what a take records, so it is counted.
        let keep = ch * frames * 4;
        if rx.slots() > keep {
            let lost = (rx.slots() - keep).div_ceil(ch);
            for _ in 0..lost * ch {
                let _ = rx.pop();
            }
            self.in_read += lost as u64;
            self.status.input_dropped.fetch_add(lost as u64, Ordering::Relaxed);
        }
        true
    }

    /// How old the input is when the engine reads it (frames between its
    /// capture and the start of the block), from the callbacks' stamps.
    fn input_queue_frames(&self) -> Option<i64> {
        let (t_r, bs) = (self.in_block_nanos?, self.block_start_nanos()?);
        Some(((bs as i64 - t_r) as f64 * self.sample_rate / 1e9).round() as i64)
    }

    fn process_block(&mut self, frames: usize) {
        let started = Instant::now();
        aitrack_dsp::disable_denormals();
        // Plugins asking (CLAP thread-check) see this thread as an audio thread.
        let _audio = aitrack_dsp::AudioThreadScope::enter();
        self.install_pool();
        self.install_graphs();
        #[cfg(test)]
        if self.fail_blocks > 0 {
            self.fail_blocks -= 1;
            panic!("injected panic in the block");
        }
        #[cfg(test)]
        if let Some(hook) = self.hook_after_install.as_mut() {
            hook();
        }
        self.apply_commands();
        let has_input = self.read_input(frames);
        self.rec_update();
        self.read_midi(frames);
        self.out.set_frames(frames);
        self.out.clear();
        let sr = self.sample_rate;

        let (in_block_nanos, block_start) = (self.in_block_nanos, self.block_start_nanos());
        let Some(g) = self.graph.as_deref_mut() else {
            self.status.playing.store(self.playing, Ordering::Relaxed);
            return;
        };
        let lat = g.total_latency as i64;
        let queue = (|| {
            let (t_r, bs) = (in_block_nanos?, block_start?);
            Some(((bs as i64 - t_r) as f64 * sr / 1e9).round() as i64)
        })();
        let loop_samples = self.loop_range.map(|(a, b)| {
            let ctx = TimeCtx { tempo: &g.tempo, sr, seg_start: 0 };
            (ctx.sample_of(a), ctx.sample_of(b))
        });
        let mut cur = 0usize;
        while cur < frames {
            let remaining = frames - cur;
            let counting = self.count_in_left > 0;
            if self.playing && !counting
                && let Some((ls, le)) = loop_samples
                    && self.pos == le && le > ls {
                        self.pos = ls;
                        self.rec.wrapped(&self.status);
                        g.release_all_notes();
                    }
            let seg = if counting {
                remaining.min(self.count_in_left as usize)
            } else if self.playing {
                match loop_samples {
                    Some((ls, le)) if le > ls && self.pos < le && self.pos + remaining as i64 > le => (le - self.pos) as usize,
                    _ => remaining,
                }
            } else {
                remaining
            };
            let playing_seg = self.playing && !counting;
            if playing_seg {
                self.run.note(self.pos, loop_samples);
            } else {
                self.run.stopped();
            }
            let input = if has_input { Some((&self.input, cur)) } else { None };
            let raw = if has_input { Some((&self.raw_in[..frames * self.raw_ch], self.raw_ch)) } else { None };
            let mut io = SegmentIo {
                seg_start: self.pos,
                frames: seg,
                playing: playing_seg,
                recording: self.recording,
                input,
                raw,
                out: &mut self.out,
                out_offset: cur,
                loop_range: self.loop_range,
            };
            g.process_segment(&mut io, &mut self.pool);

            // Metronome (aligned with what is heard: engine time − latency).
            if self.metronome && (playing_seg || counting) {
                if counting {
                    let tick = g.tempo.samples_to_tick(self.pos as f64, sr).floor() as Tick;
                    let sig = g.tempo.signature_at(tick);
                    let beat = (crate::automation::beat_ticks(sig.denominator) as f64 / PPQ as f64) * 60.0 / g.tempo.bpm_at(tick) * sr;
                    let elapsed = (self.count_in_total - self.count_in_left) as f64;
                    let mut k = (elapsed / beat).ceil();
                    while k * beat < elapsed + seg as f64 {
                        let off = (k * beat - elapsed).round().max(0.0) as usize;
                        if off < seg {
                            self.click.trigger(off, (k as i64) % sig.numerator.max(1) as i64 == 0);
                        }
                        k += 1.0;
                    }
                } else {
                    let a = self.pos - lat;
                    let b = a + seg as i64;
                    let ctx = TimeCtx { tempo: &g.tempo, sr, seg_start: a };
                    let ta = ctx.tick_of(a.max(0)).floor() as Tick;
                    let sig = g.tempo.signature_at(ta);
                    let sig_start = g.tempo.signatures.iter().rev().find(|(s, _)| *s <= ta).map_or(0, |(s, _)| *s);
                    let bl = crate::automation::beat_ticks(sig.denominator);
                    for k in ((ta - sig_start).div_euclid(bl)..).take(64) {
                        let bt = sig_start + k * bl;
                        let s = ctx.sample_of(bt);
                        if s >= b {
                            break;
                        }
                        if s >= a && bt >= 0 {
                            self.click.trigger((s - a) as usize, k.rem_euclid(sig.numerator.max(1) as i64) == 0);
                        }
                    }
                }
                self.click.render(&mut self.out, cur, cur + seg, sr);
            } else if self.click.env > 1e-4 {
                self.click.render(&mut self.out, cur, cur + seg, sr);
            }

            if counting {
                self.count_in_left -= seg as i64;
                if self.count_in_left <= 0 {
                    self.count_in_left = 0;
                    self.playing = true;
                    if self.recording && !self.rec.capturing {
                        // The take starts on this very frame, at the position the
                        // count-in was counting towards.
                        let sel = self.q.input_rx.as_ref().map_or((0, 2), |i| (i.first, i.count));
                        self.rec.begin(g, &self.status, self.pos, sr, cur + seg, sel, queue);
                    }
                }
            } else if playing_seg {
                self.pos += seg as i64;
                self.run.advanced(self.pos);
                if let Some((ls, le)) = loop_samples
                    && self.pos == le && le > ls {
                        self.pos = ls;
                        self.rec.wrapped(&self.status);
                        g.release_all_notes();
                    }
            }
            cur += seg;
        }

        self.last_end_pos = self.pos;
        self.rec.audio_block(&self.status, &self.raw_in[..frames * self.raw_ch], self.raw_ch, frames);
        self.rec.settle(&self.status);
        if self.status.tap.wants_main() {
            self.status.tap.write(self.out.channel(0), self.out.channel(1));
        }
        let engine_ticks = g.tempo.samples_to_tick(self.pos as f64, sr);
        let audible = if self.playing { g.tempo.samples_to_tick((self.pos - lat) as f64, sr) } else { engine_ticks };
        g.publish(engine_ticks);
        g.clear_live();
        self.status.midi_dropped.store(g.midi_dropped, Ordering::Relaxed);
        let (fallen, repaired) = g.take_faults();
        if fallen > 0 {
            self.status.node_panics.fetch_add(fallen as u64, Ordering::Relaxed);
        }
        if repaired > 0 {
            self.status.sanitized_samples.fetch_add(repaired, Ordering::Relaxed);
        }
        let playing_now = self.playing && self.count_in_left == 0;
        if !playing_now {
            self.run.stopped();
        }
        let at_ticks = |s: i64| g.tempo.samples_to_tick((s - lat) as f64, sr);
        let anchor = PlayheadAnchor {
            playing: playing_now,
            ticks: audible,
            ticks_per_second: g.tempo.bpm_at(audible.max(0.).floor() as Tick) * PPQ as f64 / 60.,
            at_nanos: self.cb_nanos + ((self.cb_offset + frames) as f64 / sr * 1e9) as u64,
            run_start: if playing_now { at_ticks(self.run.start) } else { audible },
            prev_run_end: if playing_now { self.run.prev_end.map(at_ticks) } else { None },
        };
        self.cb_offset += frames;
        let st = &self.status;
        st.anchor.publish(&anchor);
        st.playing.store(self.playing || self.count_in_left > 0, Ordering::Relaxed);
        st.recording.store(self.recording, Ordering::Relaxed);
        st.metronome.store(self.metronome, Ordering::Relaxed);
        st.set_position(audible, self.pos);
        let (ls, le) = self.loop_range.unwrap_or((-1, -1));
        st.loop_start.store(ls, Ordering::Relaxed);
        st.loop_end.store(le, Ordering::Relaxed);
        let budget = frames as f64 / sr;
        if budget > 0.0 {
            st.set_cpu((started.elapsed().as_secs_f64() / budget) as f32);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fallback::FallbackFactory;
    use crate::{Engine, EngineConfig};
    use aitrack_model::Project;

    #[test]
    fn run_tracker_tells_wraps_from_jumps() {
        let mut r = RunTracker::default();
        r.note(100, None);
        assert_eq!((r.start, r.prev_end), (100, None), "play start");
        r.advanced(200);
        r.note(200, Some((0, 400)));
        assert_eq!(r.start, 100, "continuous");
        r.advanced(400);
        r.note(0, Some((0, 400)));
        assert_eq!((r.start, r.prev_end), (0, Some(400)), "loop wrap");
        r.advanced(50);
        r.note(300, Some((0, 400)));
        assert_eq!((r.start, r.prev_end), (300, None), "seek");
        r.stopped();
        r.note(300, None);
        assert_eq!((r.start, r.prev_end), (300, None), "restart at the same place");
    }

    #[test]
    fn playhead_anchor_follows_blocks_and_loop_wraps() {
        let sr = 48_000.;
        let (h, proc_) = Engine::new(EngineConfig::headless(sr), Arc::new(FallbackFactory)).unwrap();
        let mut proc_ = proc_.unwrap();
        h.update_project(&Project::new("t"));
        h.flush();
        // 120 bpm: one beat = 24 000 samples = PPQ ticks.
        h.set_loop(Some((0, PPQ)));
        h.play();
        let mut buf = AudioBuffer::new(2, 1_000);
        let ticks_of = |samples: f64| samples / 24_000. * PPQ as f64;
        proc_.begin_callback();
        proc_.process_planar(&mut buf);
        let a = h.playhead_anchor();
        assert!(a.playing);
        assert!((a.ticks - ticks_of(1_000.)).abs() < 1e-6, "{a:?}");
        assert!((a.ticks_per_second - 2. * PPQ as f64).abs() < 1e-9);
        assert_eq!((a.run_start, a.prev_run_end), (0., None));
        // The block's end is due 1 000 frames after the callback started.
        let due = a.at_nanos - proc_.cb_nanos;
        assert!((due as f64 - 1_000. / sr * 1e9).abs() < 1e3, "{due}");
        for _ in 0..27 {
            proc_.process_planar(&mut buf);
        }
        // 28 000 samples into a 24 000-sample loop: wrapped once.
        let a = h.playhead_anchor();
        assert!((a.ticks - ticks_of(4_000.)).abs() < 1e-6, "{a:?}");
        assert_eq!(a.prev_run_end, Some(PPQ as f64));
        assert_eq!(a.run_start, 0.);
        // 100 ms of output latency puts the speakers before the wrap, at the loop end.
        let heard = a.heard_at(a.at_nanos, 0.1);
        let back = 0.1 * a.ticks_per_second;
        assert!((heard - (PPQ as f64 - (back - a.ticks))).abs() < 1e-6, "{heard}");
        h.stop();
        proc_.process_planar(&mut buf);
        assert!(!h.playhead_anchor().playing);
    }
}
