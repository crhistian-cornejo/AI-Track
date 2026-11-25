//! Recording: live MIDI notes and audio input captured while the transport
//! records.
//!
//! The audio thread never allocates, locks or does I/O here:
//! * MIDI: each live note-on / note-off of an armed instrument track is
//!   pushed, with the transport tick it happened at, into a wait-free queue
//!   ([`RecEvent`]). The control side pairs them into notes ([`MidiCollector`]).
//! * Audio: the input of each armed audio track is cut into fixed-size
//!   [`RecChunk`]s and pushed into another wait-free queue. A writer thread
//!   ([`Recorder`]) turns them into WAV files.
//!
//! # Where an event lands on the timeline
//! A musician plays against what they *hear*, so a position is the audible
//! one: the engine position the event was rendered at, minus the device
//! output latency and the graph's plugin delay compensation. Audio also
//! left the microphone `input latency` before the engine read it. The WAV of
//! a take drops its first `comp = input + output + PDC` frames, so its first
//! frame sits exactly on the take's start tick.
//!
//! # A take
//! Recording starts when the transport is playing, the record flag is on and
//! any count-in has finished (count-in bars are never recorded). It ends when
//! the transport stops or the flag drops; audio goes on for `comp` frames
//! (the tail still travelling through the input) and MIDI for the last block.
//! In a loop every pass keeps capturing: notes carry their `pass` number and
//! their position inside the loop.

use crate::graph::CompiledGraph;
use crate::status::SharedStatus;
use aitrack_model::{Tick, TrackId, TrackKind};
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use rtrb::{Consumer, Producer};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// Frames per audio chunk.
pub(crate) const REC_CHUNK: usize = 256;
/// Most tracks recorded audio at once.
pub(crate) const MAX_REC_TRACKS: usize = 64;
/// How often the writer updates the headers of the files of a take in progress.
const FLUSH_EVERY: Duration = Duration::from_secs(1);
/// Capacity of the MIDI queue (events).
pub(crate) const MIDI_QUEUE: usize = 8192;
/// Capacity of the audio queue (chunks), shared by every recorded track:
/// 4096 chunks of 256 frames is about 5 s for one track, 2.7 s for eight
/// (the writer drains it every 2 ms; this is the margin for a stalled disk).
pub(crate) const AUDIO_QUEUE: usize = 4096;

// ───────────────────────────── MIDI ─────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum RecKind {
    NoteOn { channel: u8, key: u8, velocity: f32 },
    NoteOff { channel: u8, key: u8 },
}

/// A live note event with the transport position it happened at.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct RecEvent {
    pub track: u64,
    /// Fractional tick on the timeline (inside the loop when looping).
    pub tick: f64,
    /// Loop pass of the take the event belongs to (0 = the first).
    pub pass: u32,
    /// The end of the loop the transport was in, in ticks (`NaN` = none):
    /// lets the collector end a note held across the wrap there.
    pub loop_end: f64,
    pub kind: RecKind,
}

/// Where recording stands, for the transport bar and the take collector.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RecordingInfo {
    /// The record flag set with `EngineHandle::set_recording`.
    pub armed: bool,
    /// A take is being captured right now (playing, flag on, count-in over).
    pub capturing: bool,
    /// Timeline tick the current (or last) take began at.
    pub start_tick: f64,
    /// Loop pass of the current take (0 until the first wrap).
    pub pass: u32,
    /// Takes started and fully drained since the engine started; a take is
    /// ready for `finish_*` when they are equal and above what was collected.
    pub takes_started: u64,
    pub takes_finished: u64,
    /// Armed instrument tracks whose notes would be recorded.
    pub midi_tracks: Vec<TrackId>,
    /// Armed audio tracks whose input would be recorded.
    pub audio_tracks: Vec<TrackId>,
    /// Events / audio chunks lost because a queue was full.
    pub dropped: u64,
    /// Device input frames thrown away (a backlog, or a full input queue)
    /// and frames of silence put in for input that was late. Either leaves a
    /// hole in an audio take.
    pub input_dropped_frames: u64,
    pub input_underrun_frames: u64,
}

/// A note played during a take.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RecordedNote {
    pub track: TrackId,
    pub channel: u8,
    pub key: u8,
    /// 0..=1.
    pub velocity: f32,
    /// Start on the timeline (inside the loop for a loop take).
    pub start_tick: Tick,
    /// At least 1.
    pub length_ticks: Tick,
    /// Loop pass the note was played in (always 0 without a loop).
    pub pass: u32,
}

/// What a finished MIDI take holds.
#[derive(Clone, Debug, PartialEq)]
pub struct MidiTake {
    /// The notes not yet handed out by `take_recorded_notes`, in start order.
    pub notes: Vec<RecordedNote>,
    /// Timeline tick where the take began (the transport position when
    /// recording started after the count-in).
    pub start_tick: Tick,
    /// Tick where it ended.
    pub end_tick: Tick,
    /// Loop passes the take went through (1 = no wrap).
    pub passes: u32,
}

#[derive(Clone, Copy, Debug)]
struct OpenNote {
    track: u64,
    channel: u8,
    key: u8,
    velocity: f32,
    tick: f64,
    pass: u32,
}

/// Control-side half of the MIDI queue: pairs note-ons with note-offs.
#[derive(Default)]
pub(crate) struct MidiCollector {
    pub rx: Option<Consumer<RecEvent>>,
    open: Vec<OpenNote>,
    done: Vec<(f64, RecordedNote)>,
}

fn note(o: &OpenNote, end: f64) -> (f64, RecordedNote) {
    let start = o.tick.round() as Tick;
    let length = ((end.round() as Tick) - start).max(1);
    (o.tick, RecordedNote { track: TrackId(o.track), channel: o.channel, key: o.key, velocity: o.velocity, start_tick: start, length_ticks: length, pass: o.pass })
}

impl MidiCollector {
    /// The take moves into pass `pass` of a loop that ends at `hi`: a note
    /// still held ends there, as the synth's did when the loop wrapped.
    /// Nothing starts again in the new pass unless the key is pressed again.
    fn carry_over(&mut self, pass: u32, hi: f64) {
        if hi.is_nan() {
            return;
        }
        let mut i = 0;
        while i < self.open.len() {
            if self.open[i].pass < pass {
                let o = self.open.remove(i);
                self.done.push(note(&o, hi));
            } else {
                i += 1;
            }
        }
    }

    /// Pull everything the audio thread queued.
    pub fn drain(&mut self) {
        let Some(rx) = self.rx.as_mut() else { return };
        let mut events = Vec::new();
        while let Ok(e) = rx.pop() {
            events.push(e);
        }
        for e in events {
            self.carry_over(e.pass, e.loop_end);
            match e.kind {
                RecKind::NoteOn { channel, key, velocity } => {
                    if let Some(i) = self.open.iter().position(|o| o.track == e.track && o.key == key && o.channel == channel) {
                        let o = self.open.remove(i);
                        self.done.push(note(&o, e.tick));
                    }
                    self.open.push(OpenNote { track: e.track, channel, key, velocity, tick: e.tick, pass: e.pass });
                }
                RecKind::NoteOff { channel, key } => {
                    if let Some(i) = self.open.iter().position(|o| o.track == e.track && o.key == key && o.channel == channel) {
                        let o = self.open.remove(i);
                        self.done.push(note(&o, e.tick));
                    }
                }
            }
        }
    }

    /// The notes completed so far, in start order.
    pub fn take_closed(&mut self) -> Vec<RecordedNote> {
        self.drain();
        let mut notes = std::mem::take(&mut self.done);
        notes.sort_by(|a, b| a.1.pass.cmp(&b.1.pass).then(a.0.total_cmp(&b.0)));
        notes.into_iter().map(|n| n.1).collect()
    }

    /// Everything left; notes still held end at `end_tick` of pass
    /// `end_pass` (those held through a loop wrap end at its end, `hi`).
    pub fn take_all(&mut self, end_tick: f64, end_pass: u32, hi: f64) -> Vec<RecordedNote> {
        self.drain();
        self.carry_over(end_pass, hi);
        for o in std::mem::take(&mut self.open) {
            self.done.push(note(&o, end_tick.max(o.tick)));
        }
        self.take_closed()
    }
}

// ───────────────────────────── Audio ─────────────────────────────

/// A piece of one track's input on its way to the writer thread.
#[derive(Clone, Copy)]
pub(crate) struct RecChunk {
    pub track: u64,
    pub session: u32,
    pub rate: u32,
    pub channels: u8,
    pub frames: u16,
    /// Frame index of this chunk's first frame in the file.
    pub index: u64,
    /// Timeline tick of the file's first frame.
    pub start_tick: f64,
    pub last: bool,
    /// On the closing chunk: frames of this track the queue had no room for.
    pub dropped: u64,
    pub l: [f32; REC_CHUNK],
    pub r: [f32; REC_CHUNK],
}

impl RecChunk {
    fn new(track: u64, session: u32, rate: u32, channels: u8, index: u64, start_tick: f64) -> Self {
        Self { track, session, rate, channels, frames: 0, index, start_tick, last: false, dropped: 0, l: [0.0; REC_CHUNK], r: [0.0; REC_CHUNK] }
    }
}

/// A WAV file written by a take.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioTake {
    pub track: TrackId,
    pub path: PathBuf,
    /// Timeline tick of the file's first frame (input latency compensated).
    pub start_tick: f64,
    pub frames: u64,
    pub sample_rate: u32,
    pub channels: u8,
    /// Set when the file could not be written; `frames` is then what made it.
    pub error: Option<String>,
    /// Frames the real-time queue had no room for (the writer filled them
    /// with silence to keep the file on the grid): the take has holes.
    pub dropped_frames: u64,
}

enum WriterMsg {
    Attach(Consumer<RecChunk>),
    SetDir(PathBuf),
    /// Finish every file still open (the audio thread was rebuilt mid-take).
    CloseOpen,
    Shutdown,
}

/// The non-real-time thread that writes the WAV files of audio takes.
pub(crate) struct Recorder {
    ctl: Sender<WriterMsg>,
    pub results: Arc<Mutex<Vec<AudioTake>>>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl Recorder {
    pub fn spawn() -> std::io::Result<Self> {
        let (ctl, rx) = crossbeam_channel::unbounded();
        let results = Arc::new(Mutex::new(Vec::new()));
        let r = results.clone();
        let thread = std::thread::Builder::new().name("aitrack-engine-recorder".into()).spawn(move || writer_loop(rx, r))?;
        Ok(Self { ctl, results, thread: Mutex::new(Some(thread)) })
    }

    /// Read chunks from `rx` from now on (the audio thread was rebuilt).
    pub fn attach(&self, rx: Consumer<RecChunk>) {
        let _ = self.ctl.send(WriterMsg::Attach(rx));
    }

    /// Finish the files of a take whose audio thread no longer exists.
    pub fn close_open(&self) {
        let _ = self.ctl.send(WriterMsg::CloseOpen);
    }

    pub fn set_dir(&self, dir: PathBuf) {
        let _ = self.ctl.send(WriterMsg::SetDir(dir));
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        let _ = self.ctl.send(WriterMsg::Shutdown);
        if let Some(t) = self.thread.lock().unwrap_or_else(|p| p.into_inner()).take() {
            let _ = t.join();
        }
    }
}

struct OpenFile {
    track: u64,
    session: u32,
    path: PathBuf,
    writer: Option<hound::WavWriter<std::io::BufWriter<std::fs::File>>>,
    channels: u8,
    rate: u32,
    start_tick: f64,
    written: u64,
    error: Option<String>,
    dropped: u64,
}

fn default_dir() -> PathBuf {
    std::env::temp_dir().join("aitrack-recordings")
}

fn writer_loop(ctl: Receiver<WriterMsg>, results: Arc<Mutex<Vec<AudioTake>>>) {
    let mut rx: Option<Consumer<RecChunk>> = None;
    let mut dir = default_dir();
    let mut files: Vec<OpenFile> = Vec::new();
    // Distinguishes the files of this run from an earlier one.
    let engine = ENGINES.fetch_add(1, Ordering::Relaxed);
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let mut flushed = std::time::Instant::now();
    loop {
        let mut idle = true;
        if let Some(c) = rx.as_mut() {
            while let Ok(chunk) = c.pop() {
                idle = false;
                write_chunk(&mut files, &dir, stamp, engine, &results, &chunk);
            }
        }
        // Keep the headers of open files current, so a crash mid-take
        // leaves a WAV that holds what was recorded until now.
        if flushed.elapsed() >= FLUSH_EVERY {
            flushed = std::time::Instant::now();
            for f in &mut files {
                if let Some(w) = f.writer.as_mut()
                    && let Err(e) = w.flush()
                {
                    f.error.get_or_insert(e.to_string());
                }
            }
        }
        let wait = if idle { Duration::from_millis(2) } else { Duration::ZERO };
        match ctl.recv_timeout(wait) {
            Ok(WriterMsg::Attach(new)) => {
                // Whatever the old queue still holds belongs to files already open.
                if let Some(old) = rx.as_mut() {
                    while let Ok(chunk) = old.pop() {
                        write_chunk(&mut files, &dir, stamp, engine, &results, &chunk);
                    }
                }
                rx = Some(new);
            }
            Ok(WriterMsg::SetDir(d)) => dir = d,
            Ok(WriterMsg::CloseOpen) => {
                if let Some(c) = rx.as_mut() {
                    while let Ok(chunk) = c.pop() {
                        write_chunk(&mut files, &dir, stamp, engine, &results, &chunk);
                    }
                }
                for f in files.drain(..) {
                    finish_file(f, &results);
                }
            }
            Ok(WriterMsg::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }
    }
    if let Some(c) = rx.as_mut() {
        while let Ok(chunk) = c.pop() {
            write_chunk(&mut files, &dir, stamp, engine, &results, &chunk);
        }
    }
    for f in files.drain(..) {
        finish_file(f, &results);
    }
}

fn finish_file(mut f: OpenFile, results: &Mutex<Vec<AudioTake>>) {
    if let Some(w) = f.writer.take()
        && let Err(e) = w.finalize()
    {
        f.error.get_or_insert(e.to_string());
    }
    let take = AudioTake {
        track: TrackId(f.track),
        path: f.path,
        start_tick: f.start_tick,
        frames: f.written,
        sample_rate: f.rate,
        channels: f.channels,
        error: f.error,
        dropped_frames: f.dropped,
    };
    results.lock().unwrap_or_else(|p| p.into_inner()).push(take);
}

/// Engines created so far in this process: part of every file name, so two
/// engines (or a restarted one) never write to the same name.
static ENGINES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A new file in `dir` named `take-<run>-<engine>-<take>-track-<id>[-n].wav`.
/// Never an existing one: a name in use (an earlier run, a clip that points
/// at it) gets a `-n` suffix instead of being truncated.
fn create_unique(dir: &Path, stem: &str, spec: hound::WavSpec) -> Result<(PathBuf, hound::WavWriter<std::io::BufWriter<std::fs::File>>), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    for n in 0..10_000u32 {
        let name = if n == 0 { format!("{stem}.wav") } else { format!("{stem}-{n}.wav") };
        let path = dir.join(name);
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => {
                let w = hound::WavWriter::new(std::io::BufWriter::new(file), spec).map_err(|e| e.to_string())?;
                return Ok((path, w));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    Err("no free file name".into())
}

fn open_file(dir: &Path, stamp: u64, engine: u64, c: &RecChunk) -> OpenFile {
    let stem = format!("take-{stamp}-{engine}-{}-track-{}", c.session, c.track);
    let spec = hound::WavSpec { channels: c.channels.max(1) as u16, sample_rate: c.rate, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
    let (path, writer, error) = match create_unique(dir, &stem, spec) {
        Ok((path, w)) => (path, Some(w), None),
        Err(e) => (dir.join(format!("{stem}.wav")), None, Some(e)),
    };
    OpenFile { track: c.track, session: c.session, path, writer, channels: c.channels.max(1), rate: c.rate, start_tick: c.start_tick, written: 0, error, dropped: 0 }
}

fn put_frames(f: &mut OpenFile, l: &[f32], r: &[f32]) {
    let Some(w) = f.writer.as_mut() else { return };
    for i in 0..l.len() {
        let res = if f.channels >= 2 { w.write_sample(l[i]).and_then(|()| w.write_sample(r[i])) } else { w.write_sample(l[i]) };
        if let Err(e) = res {
            f.error.get_or_insert(e.to_string());
            f.writer = None;
            return;
        }
    }
    f.written += l.len() as u64;
}

fn write_chunk(files: &mut Vec<OpenFile>, dir: &Path, stamp: u64, engine: u64, results: &Mutex<Vec<AudioTake>>, c: &RecChunk) {
    let i = match files.iter().position(|f| f.track == c.track && f.session == c.session) {
        Some(i) => i,
        None => {
            files.push(open_file(dir, stamp, engine, c));
            files.len() - 1
        }
    };
    let f = &mut files[i];
    // Chunks the queue had no room for: keep the file on the grid with silence.
    let zeros = [0.0f32; REC_CHUNK];
    let mut gap = c.index.saturating_sub(f.written).min(48_000 * 30);
    while gap > 0 {
        let n = gap.min(REC_CHUNK as u64) as usize;
        put_frames(f, &zeros[..n], &zeros[..n]);
        gap -= n as u64;
    }
    let n = c.frames as usize;
    put_frames(f, &c.l[..n], &c.r[..n]);
    if c.last {
        files[i].dropped = c.dropped;
        finish_file(files.swap_remove(i), results);
    }
}

/// Planar stereo frames `frame0..frame0 + n` of the interleaved device input
/// `raw` (`channels` per frame) for the channels `first..first + count`
/// (mono is duplicated to both sides). Out-of-range channels read as silence.
#[allow(clippy::too_many_arguments)]
pub(crate) fn extract_input(raw: &[f32], channels: usize, frame0: usize, n: usize, first: usize, count: usize, l: &mut [f32], r: &mut [f32]) {
    let ch = channels.max(1);
    let mono = count < 2 || first + 1 >= ch;
    for f in 0..n {
        let base = (frame0 + f) * ch;
        let left = raw.get(base + first).copied().filter(|_| first < ch).unwrap_or(0.0);
        l[f] = left;
        r[f] = if mono { left } else { raw.get(base + first + 1).copied().unwrap_or(0.0) };
    }
}

// ───────────────────────────── Audio-thread state ─────────────────────────────

/// One track whose input is being recorded.
#[derive(Clone, Copy)]
pub(crate) struct CapTrack {
    pub id: u64,
    pub first: usize,
    pub count: usize,
    pub channels: u8,
    /// Frames still to drop at the head (the compensated latency).
    pub trim: i64,
    /// Frames sent to the writer so far.
    pub sent: u64,
    /// Frames of this track the queue had no room for.
    pub dropped: u64,
}

/// A track whose closing chunk has not found room in the queue yet.
#[derive(Clone, Copy)]
struct Closing {
    id: u64,
    session: u32,
    channels: u8,
    sent: u64,
    dropped: u64,
    start_tick: f64,
}

/// The audio thread's recording state. Everything is preallocated.
pub(crate) struct RecState {
    pub capturing: bool,
    /// The block after a stop still takes MIDI that happened before it.
    pub midi_tail: bool,
    /// Audio chunks are flowing (until the last one is sent).
    pub audio_open: bool,
    pub pending_end: bool,
    pub start_pos: i64,
    pub end_pos: i64,
    pub pass: u32,
    pub session: u32,
    /// Frames of input latency + output latency + PDC to trim at the head.
    pub comp: i64,
    /// First frame of the current block that belongs to the take.
    pub audio_from: usize,
    pub tail_left: i64,
    pub start_tick: f64,
    pub tracks: Vec<CapTrack>,
    closing: Vec<Closing>,
    pub midi_tx: Option<Producer<RecEvent>>,
    /// Notes played while not recording (Capture MIDI); a full queue just drops them.
    pub cap_tx: Option<Producer<RecEvent>>,
    pub audio_tx: Option<Producer<RecChunk>>,
    scratch_l: Vec<f32>,
    scratch_r: Vec<f32>,
    pub rate: u32,
}

impl RecState {
    pub fn new(max_frames: usize, rate: u32) -> Self {
        Self {
            capturing: false,
            midi_tail: false,
            audio_open: false,
            pending_end: false,
            start_pos: 0,
            end_pos: 0,
            pass: 0,
            session: 0,
            comp: 0,
            audio_from: 0,
            tail_left: 0,
            start_tick: 0.0,
            tracks: Vec::with_capacity(MAX_REC_TRACKS),
            closing: Vec::with_capacity(2 * MAX_REC_TRACKS),
            midi_tx: None,
            cap_tx: None,
            audio_tx: None,
            scratch_l: vec![0.0; max_frames.max(REC_CHUNK)],
            scratch_r: vec![0.0; max_frames.max(REC_CHUNK)],
            rate,
        }
    }

    /// A take starts at engine position `pos`; `block_from` is the frame of
    /// the current block it starts at. `default_sel` is the device channels
    /// of an audio track that picked no input of its own.
    #[allow(clippy::too_many_arguments)]
    pub fn begin(&mut self, g: &CompiledGraph, status: &SharedStatus, pos: i64, sr: f64, block_from: usize, default_sel: (usize, usize), queue_frames: Option<i64>) {
        if self.audio_open || self.midi_tail {
            // The previous take is still draining: close it at once.
            self.close_audio();
            self.midi_tail = false;
            if self.pending_end {
                // Counted as ended now; closing chunks that found no room go on in settle().
                self.pending_end = false;
                status.rec_ended.fetch_add(1, Ordering::Release);
            }
        }
        self.tracks.clear();
        let lat = status.io.snapshot(self.rate, g.total_latency);
        // The input's age when the engine reads it: measured from the stamps of
        // the input callbacks when there are (it differs from take to take with
        // how full the queue happens to be), else the driver's figures.
        let input = queue_frames.unwrap_or_else(|| {
            if lat.input_frames == 0 && lat.input_buffer_frames == 0 { 0 } else { lat.input_frames.max(lat.input_buffer_frames) as i64 }
        });
        self.comp = (input + lat.output_frames as i64 + g.total_latency as i64).max(0);
        for t in &g.tracks {
            if t.arm && t.kind == TrackKind::Audio && !t.no_input && self.tracks.len() < MAX_REC_TRACKS {
                let (first, count) = t.input_sel.map_or(default_sel, |(f, c)| (f as usize, c as usize));
                self.tracks.push(CapTrack { id: t.id.0, first, count, channels: if count >= 2 { 2 } else { 1 }, trim: self.comp, sent: 0, dropped: 0 });
            }
        }
        // Take numbers are the status counter's, so they never repeat while the
        // engine lives, whatever audio thread (re)built the state.
        self.session = (status.rec_sessions.fetch_add(1, Ordering::Release) + 1) as u32;
        self.capturing = true;
        self.pending_end = true;
        self.start_pos = pos;
        self.pass = 0;
        self.audio_from = block_from;
        self.audio_open = !self.tracks.is_empty();
        self.tail_left = self.comp;
        self.start_tick = g.tempo.samples_to_tick(pos as f64, sr);
        status.rec_start_tick.store(self.start_tick.to_bits(), Ordering::Relaxed);
        status.rec_pass.store(0, Ordering::Relaxed);
        status.rec_capturing.store(true, Ordering::Release);
    }

    /// The take stops; `end_pos` is where the last block ended.
    pub fn end(&mut self, g: &CompiledGraph, status: &SharedStatus, end_pos: i64, sr: f64) {
        self.capturing = false;
        self.midi_tail = true;
        self.end_pos = end_pos;
        let tick = g.tempo.samples_to_tick(end_pos as f64, sr);
        status.rec_end_tick.store(tick.to_bits(), Ordering::Relaxed);
        status.rec_capturing.store(false, Ordering::Release);
    }

    /// A loop wrapped while recording.
    pub fn wrapped(&mut self, status: &SharedStatus) {
        if self.capturing {
            self.pass += 1;
            status.rec_pass.store(self.pass, Ordering::Relaxed);
        }
    }

    /// Queue a note event; counts it as dropped if the queue is full.
    pub fn push_event(&mut self, status: &SharedStatus, e: RecEvent) {
        if let Some(tx) = self.midi_tx.as_mut()
            && tx.push(e).is_ok()
        {
            return;
        }
        status.rec_dropped.fetch_add(1, Ordering::Relaxed);
    }

    /// Queue a note event played while not recording; wait-free, and a full
    /// queue drops it (the capture buffer only keeps the recent past).
    pub fn push_capture(&mut self, e: RecEvent) {
        if let Some(tx) = self.cap_tx.as_mut() {
            let _ = tx.push(e);
        }
    }

    /// Send the block's input of every recorded track (`frames` frames of
    /// the interleaved device input `raw`, `channels` per frame).
    pub fn audio_block(&mut self, status: &SharedStatus, raw: &[f32], channels: usize, frames: usize) {
        if !self.audio_open {
            return;
        }
        let (from, to) = if self.capturing {
            let from = std::mem::take(&mut self.audio_from).min(frames);
            (from, frames)
        } else {
            let n = (frames as i64).min(self.tail_left.max(0)) as usize;
            self.tail_left -= n as i64;
            (0, n)
        };
        let len = to.saturating_sub(from);
        if len > 0 {
            for ti in 0..self.tracks.len() {
                let t = self.tracks[ti];
                extract_input(raw, channels, from, len, t.first, t.count, &mut self.scratch_l[..len], &mut self.scratch_r[..len]);
                // Drop the head frames the latency compensation removes.
                let skip = (t.trim.max(0) as usize).min(len);
                self.tracks[ti].trim -= skip as i64;
                let mut at = skip;
                while at < len {
                    let n = (len - at).min(REC_CHUNK);
                    let mut c = RecChunk::new(t.id, self.session, self.rate, t.channels, self.tracks[ti].sent, self.start_tick);
                    c.frames = n as u16;
                    c.l[..n].copy_from_slice(&self.scratch_l[at..at + n]);
                    c.r[..n].copy_from_slice(&self.scratch_r[at..at + n]);
                    if !self.send_chunk(status, c) {
                        self.tracks[ti].dropped += n as u64;
                    }
                    self.tracks[ti].sent += n as u64;
                    at += n;
                }
            }
        }
        if !self.capturing && self.tail_left <= 0 {
            self.close_audio();
        }
    }

    fn send_chunk(&mut self, status: &SharedStatus, c: RecChunk) -> bool {
        if let Some(tx) = self.audio_tx.as_mut()
            && tx.push(c).is_ok()
        {
            return true;
        }
        status.rec_dropped.fetch_add(1, Ordering::Relaxed);
        false
    }

    /// The take's audio is complete: queue the closing chunk of each file.
    fn close_audio(&mut self) {
        if !self.audio_open {
            return;
        }
        self.audio_open = false;
        for t in &self.tracks {
            if self.closing.len() < self.closing.capacity() {
                self.closing.push(Closing { id: t.id, session: self.session, channels: t.channels, sent: t.sent, dropped: t.dropped, start_tick: self.start_tick });
            }
        }
        self.flush_closing();
    }

    /// Send the closing chunks that have not found room yet; a full queue
    /// leaves them for the next block (a file must never stay open).
    fn flush_closing(&mut self) {
        while let Some(c) = self.closing.first().copied() {
            let Some(tx) = self.audio_tx.as_mut() else {
                self.closing.clear();
                return;
            };
            let mut chunk = RecChunk::new(c.id, c.session, self.rate, c.channels, c.sent, c.start_tick);
            chunk.last = true;
            chunk.dropped = c.dropped;
            if tx.push(chunk).is_err() {
                return;
            }
            self.closing.remove(0);
        }
    }

    /// Count a take as ended once MIDI and audio have both drained.
    pub fn settle(&mut self, status: &SharedStatus) {
        self.flush_closing();
        if self.pending_end && !self.capturing && !self.midi_tail && !self.audio_open && self.closing.is_empty() {
            self.pending_end = false;
            status.rec_ended.fetch_add(1, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rt::{InputSink, InputStamp};
    use rtrb::RingBuffer;

    fn state(capacity: usize) -> (RecState, Consumer<RecChunk>, SharedStatus) {
        let (tx, rx) = RingBuffer::new(capacity);
        let mut s = RecState::new(1024, 48_000);
        s.audio_tx = Some(tx);
        (s, rx, SharedStatus::new())
    }

    fn track() -> CapTrack {
        CapTrack { id: 7, first: 0, count: 2, channels: 2, trim: 0, sent: 0, dropped: 0 }
    }

    #[test]
    fn a_closing_mark_that_finds_no_room_is_retried_until_it_does() {
        let (mut s, mut rx, status) = state(1);
        s.tracks.push(track());
        s.tracks.push(CapTrack { id: 8, ..track() });
        s.audio_open = true;
        s.pending_end = true;
        // A chunk occupies the only slot.
        s.send_chunk(&status, RecChunk::new(1, 1, 48_000, 2, 0, 0.0));
        s.close_audio();
        s.settle(&status);
        assert_eq!(status.rec_ended.load(Ordering::Relaxed), 0, "not ended while a file may stay open");
        let mut closed = Vec::new();
        for _ in 0..6 {
            while let Ok(c) = rx.pop() {
                if c.last {
                    closed.push(c.track);
                }
            }
            s.settle(&status);
        }
        assert_eq!(closed, vec![7, 8], "both files get their closing chunk, in order");
        assert_eq!(status.rec_ended.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn frames_the_queue_had_no_room_for_are_counted_per_track() {
        let (mut s, mut rx, status) = state(2);
        s.tracks.push(track());
        s.audio_open = true;
        s.capturing = true;
        let raw = vec![0.25f32; 2 * 1024];
        // 1024 frames = 4 chunks into a queue of 2.
        s.audio_block(&status, &raw, 2, 1024);
        assert_eq!(s.tracks[0].dropped, 512);
        assert_eq!(s.tracks[0].sent, 1024, "the file's frame index goes on, the writer fills the hole");
        assert_eq!(status.rec_dropped.load(Ordering::Relaxed), 2);
        while rx.pop().is_ok() {}
        s.capturing = false;
        s.tail_left = 0;
        s.close_audio();
        let last = rx.pop().expect("closing chunk");
        assert!(last.last && last.dropped == 512 && last.index == 1024);
    }

    #[test]
    fn the_writer_pads_holes_with_silence_and_reports_them() {
        let dir = std::env::temp_dir().join(format!("aitrack-writer-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let results = Mutex::new(Vec::new());
        let mut files = Vec::new();
        let mut c = RecChunk::new(3, 1, 48_000, 1, 0, 12.0);
        c.frames = 100;
        c.l[..100].fill(0.5);
        write_chunk(&mut files, &dir, 1, 1, &results, &c);
        // 300 frames never arrived; the closing chunk says so.
        let mut last = RecChunk::new(3, 1, 48_000, 1, 400, 12.0);
        last.last = true;
        last.dropped = 300;
        write_chunk(&mut files, &dir, 1, 1, &results, &last);
        let take = results.lock().unwrap().pop().expect("the file was finished");
        assert_eq!((take.frames, take.dropped_frames, take.error.clone()), (400, 300, None));
        assert_eq!(take.start_tick, 12.0);
        let mut r = hound::WavReader::open(&take.path).unwrap();
        let s: Vec<f32> = r.samples::<f32>().map(|s| s.unwrap()).collect();
        assert_eq!((s.len(), s[99], s[100], s[399]), (400, 0.5, 0.0, 0.0));
        // The same name again never overwrites a file.
        let mut again = RecChunk::new(3, 1, 48_000, 1, 0, 0.0);
        again.frames = 10;
        again.last = true;
        write_chunk(&mut files, &dir, 1, 1, &results, &again);
        let second = results.lock().unwrap().pop().unwrap();
        assert_ne!(second.path, take.path);
        assert_eq!(hound::WavReader::open(&take.path).unwrap().len(), 400, "the first file is intact");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_take_in_progress_is_a_readable_wav_on_disk() {
        // What a crash mid-take leaves behind must hold what was recorded so far.
        let dir = std::env::temp_dir().join(format!("aitrack-writer-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let rec = Recorder::spawn().unwrap();
        rec.set_dir(dir.clone());
        let (mut tx, rx) = RingBuffer::<RecChunk>::new(8);
        rec.attach(rx);
        let mut c = RecChunk::new(5, 1, 48_000, 2, 0, 0.0);
        c.frames = 200;
        c.l[..200].fill(0.25);
        c.r[..200].fill(-0.25);
        assert!(tx.push(c).is_ok());
        let deadline = std::time::Instant::now() + FLUSH_EVERY * 4;
        let mut seen = 0;
        while std::time::Instant::now() < deadline && seen < 200 {
            std::thread::sleep(Duration::from_millis(20));
            let file = std::fs::read_dir(&dir).ok().and_then(|mut d| d.next()).and_then(|e| e.ok()).map(|e| e.path());
            seen = file.and_then(|p| hound::WavReader::open(p).ok()).map_or(0, |r| r.duration());
        }
        assert_eq!(seen, 200, "the open file's header counts the frames written so far");
        drop(rec);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_input_queue_takes_whole_frames_only() {
        let (samples, mut rx) = RingBuffer::new(5);
        let (stamps, _s) = RingBuffer::<InputStamp>::new(4);
        let mut sink = InputSink { samples, stamps, written: 0 };
        let data = [1.0f32, -1.0, 2.0, -2.0, 3.0, -3.0];
        let kept = sink.push_with(3, 2, 99, |i| data[i]);
        assert_eq!(kept, 2, "room for five samples: two whole frames, not two and a half");
        let mut got = Vec::new();
        while let Ok(v) = rx.pop() {
            got.push(v);
        }
        assert_eq!(got, vec![1.0, -1.0, 2.0, -2.0], "left and right stay paired");
        assert_eq!(sink.written, 2);
    }
}
