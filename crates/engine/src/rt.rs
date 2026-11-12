//! The audio-thread side of the engine.
//!
//! [`AudioProcessor`] is what the device callback (or a test, or the
//! offline renderer) calls once per buffer. Per block it:
//! 1. installs a newly compiled graph if one is waiting *and* the retire
//!    queue has room (the old graph goes back to be dropped elsewhere);
//! 2. applies transport / launch commands and live MIDI;
//! 3. splits the block at loop wraps and count-in end, and processes each
//!    segment through the graph;
//! 4. adds the metronome and publishes meters, position, clip states, CPU.
//!
//! Nothing here allocates, locks or does I/O.

use crate::clip::TimeCtx;
use crate::graph::{CompiledGraph, SegmentIo};
use crate::status::{PlayheadAnchor, SharedStatus};
use aitrack_dsp::{AudioBuffer, EventKind};
use aitrack_model::{Quantize, SceneId, TempoMap, Tick, TrackId, TrackKind, PPQ};
use rtrb::{Consumer, Producer};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

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
    pub midi_rx: Vec<Consumer<MidiMessage>>,
    /// Interleaved capture samples.
    pub input_rx: Option<InputQueue>,
}

pub(crate) struct InputQueue {
    pub rx: Consumer<f32>,
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

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    /// Engine position in samples (before PDC compensation).
    pub fn position_samples(&self) -> i64 {
        self.pos
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
                        g.push_live(t, event);
                    }
                }
                RtCommand::SetGlobalQuantize(q) => self.global_quantize = q,
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

    fn read_midi(&mut self) {
        let Some(g) = &mut self.graph else {
            for rx in &mut self.q.midi_rx {
                while rx.pop().is_ok() {}
            }
            return;
        };
        let target = self.midi_target.and_then(|t| g.track_index(t));
        for rx in &mut self.q.midi_rx {
            while let Ok(msg) = rx.pop() {
                let Some(ev) = msg.to_event() else { continue };
                match target {
                    Some(t) => g.push_live(t, ev),
                    None => {
                        for ti in 0..g.tracks.len() {
                            let t = &g.tracks[ti];
                            if t.arm && t.kind == TrackKind::Instrument {
                                g.push_live(ti, ev);
                            }
                        }
                    }
                }
            }
        }
    }

    fn read_input(&mut self, frames: usize) -> bool {
        let Some(InputQueue { rx, channels, first, count }) = &mut self.q.input_rx else { return false };
        let ch = (*channels).max(1);
        let (first, mono) = ((*first).min(ch - 1), *count < 2 || *first + 1 >= ch);
        self.input.set_frames(frames);
        let avail = rx.slots() / ch;
        let take = avail.min(frames);
        let (l, r) = self.input.stereo_mut();
        for f in 0..frames {
            if f < take {
                let mut frame = [0f32; 2];
                for c in 0..ch {
                    let s = rx.pop().unwrap_or(0.0);
                    if c == first {
                        frame[0] = s;
                    } else if c == first + 1 {
                        frame[1] = s;
                    }
                }
                l[f] = frame[0];
                r[f] = if mono { frame[0] } else { frame[1] };
            } else {
                l[f] = 0.0;
                r[f] = 0.0;
            }
        }
        // Drop a backlog so monitoring latency cannot grow.
        while rx.slots() > ch * frames * 4 {
            let _ = rx.pop();
        }
        true
    }

    fn process_block(&mut self, frames: usize) {
        let started = Instant::now();
        aitrack_dsp::disable_denormals();
        self.install_graphs();
        self.apply_commands();
        self.read_midi();
        let has_input = self.read_input(frames);
        self.out.set_frames(frames);
        self.out.clear();
        let sr = self.sample_rate;

        let Some(g) = self.graph.as_deref_mut() else {
            self.status.playing.store(self.playing, Ordering::Relaxed);
            return;
        };
        let lat = g.total_latency as i64;
        let loop_samples = self.loop_range.map(|(a, b)| {
            let ctx = TimeCtx { tempo: &g.tempo, sr, seg_start: 0 };
            (ctx.sample_of(a), ctx.sample_of(b))
        });
        let mut cur = 0usize;
        let mut first = true;
        while cur < frames {
            let remaining = frames - cur;
            let counting = self.count_in_left > 0;
            if self.playing && !counting
                && let Some((ls, le)) = loop_samples
                    && self.pos == le && le > ls {
                        self.pos = ls;
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
            let mut io = SegmentIo {
                seg_start: self.pos,
                frames: seg,
                playing: playing_seg,
                recording: self.recording,
                first,
                input,
                out: &mut self.out,
                out_offset: cur,
                loop_range: self.loop_range,
            };
            g.process_segment(&mut io);

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
                }
            } else if playing_seg {
                self.pos += seg as i64;
                self.run.advanced(self.pos);
                if let Some((ls, le)) = loop_samples
                    && self.pos == le && le > ls {
                        self.pos = ls;
                        g.release_all_notes();
                    }
            }
            cur += seg;
            first = false;
        }

        if self.status.tap.wants_main() {
            self.status.tap.write(self.out.channel(0), self.out.channel(1));
        }
        let engine_ticks = g.tempo.samples_to_tick(self.pos as f64, sr);
        let audible = if self.playing { g.tempo.samples_to_tick((self.pos - lat) as f64, sr) } else { engine_ticks };
        g.publish(engine_ticks);
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
