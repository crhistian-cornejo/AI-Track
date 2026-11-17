//! The compiled, immutable-shape audio graph and its real-time processing.
//!
//! A [`CompiledGraph`] is built by [`crate::compile`] on the control thread
//! and then owned exclusively by the audio thread. Its *shape* never changes
//! there (no allocation); only runtime state (envelopes, active notes,
//! session playback, smoothers) mutates. When a new graph arrives, the audio
//! thread moves that runtime state over ([`CompiledGraph::adopt`]) and sends
//! the old graph back to be dropped off the audio thread.
//!
//! Per track, one segment is processed as:
//! clip player → device chain (racks, sub-blocked for automation) →
//! pre-fader sends → volume/pan/mute (ramped every 32 samples) →
//! post-fader sends → meter → PDC delay → destination input.

use crate::automation::{lane_value, pan_from_normalized, pan_gains, volume_db_from_normalized, CONTROL_BLOCK};
use crate::clip::{ActiveNotes, CompiledClip, EventList, Rng, TimeCtx};
use crate::delay::DelayLine;
use crate::shared::SharedProcessor;
use crate::status::{MeterBank, SessionStatus, NO_SCENE, STOP_SCENE};
use crate::tap::{AnalysisTap, DeviceMeter, DEVICE_METER_LEN};
use aitrack_dsp::{db_to_gain, AudioBuffer, Event, EventKind, EventSink, MidiContext, ProcessContext, Transport};
use aitrack_model::{AutomationLane, DeviceId, FollowAction, LaunchMode, Monitor, Quantize, RackKind, SceneId, TempoMap, Tick, TrackId, TrackKind, PPQ};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub(crate) const EVENT_CAPACITY: usize = 2048;

/// Run `f`, turning a panic into `None`. Free (a landing pad, no work) when
/// nothing panics. The payload is leaked on purpose: freeing it would be a
/// heap operation on the audio thread, and a device only falls once.
#[inline]
pub(crate) fn guarded<R>(f: impl FnOnce() -> R) -> Option<R> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => Some(r),
        Err(payload) => {
            std::mem::forget(payload);
            None
        }
    }
}

// ───────────────────────────── Devices ─────────────────────────────

pub(crate) enum DeviceNode {
    Proc(ProcNode),
    Rack(RackNode),
    /// A device no factory could create: passes audio through.
    Missing(DeviceId),
}

pub(crate) struct ProcNode {
    pub id: DeviceId,
    pub proc: Arc<SharedProcessor>,
    pub enabled: Arc<AtomicBool>,
    pub init_params: Vec<(u32, f32)>,
    pub is_instrument: bool,
    /// Display data (`Processor::meter`) for the UI.
    pub meter: Arc<DeviceMeter>,
    /// Which sidechain buffer (see [`CompiledGraph::side`]) this device listens to.
    pub sidechain: Option<usize>,
    /// Automation of a hosted plugin's parameters for the current block, as
    /// timed `Param` events (offsets in the whole block); see [`ParamTarget::Timed`].
    /// Capacity is set at compile time, only for devices that need it.
    pub auto_events: EventList,
    /// The block's events merged with `auto_events` for one process call.
    pub merged: EventList,
    /// Consecutive samples of `-140 dB` silence produced while receiving silent input.
    pub silent_samples: u32,
    /// True once `silent_samples` passes `max(proc.tail(), guard)` and no
    /// input/events/automation are present; skips `proc.process` until woken.
    pub sleeping: bool,
    /// Scratch for a stock device whose parameters are automated: it is
    /// processed in short pieces, the parameter changed between them, while
    /// the rest of the chain (hosted plugins included) keeps whole blocks.
    pub chunk: Option<Box<ChunkScratch>>,
    /// A MIDI effect (`Category::MidiEffect`): transforms the events of the
    /// chain and is not given audio.
    pub is_midi: bool,
    pub midi: Option<Box<MidiState>>,
}

/// Marks a key / channel slot of [`MidiState::sounding`] as silent.
const NOT_SOUNDING: i32 = i32::MIN;

/// Engine-side state of a MIDI effect node.
pub(crate) struct MidiState {
    /// The events this device handed on for the current block.
    pub out: EventList,
    /// Events lost because `out` was full (the device's own queue counts its
    /// own, see `Processor::dropped_events`).
    pub dropped: u32,
    /// For each channel × key (`channel * 128 + key`), the `note_id` of a
    /// note the device started and has not ended; `NOT_SOUNDING` otherwise.
    /// What a flush closes, whatever the device does.
    pub sounding: Vec<i32>,
    /// Close everything open and reset the device at the next block: a new
    /// graph, the transport stopped or jumped.
    pub flush: bool,
    /// The device was enabled in the last block it ran.
    pub was_enabled: bool,
}

impl MidiState {
    pub fn new() -> Self {
        Self { out: EventList::with_capacity(EVENT_CAPACITY), dropped: 0, sounding: vec![NOT_SOUNDING; 16 * 128], flush: true, was_enabled: true }
    }

    /// NoteOffs at offset 0 for every note still open; forgets them.
    fn close_open(&mut self) {
        for (i, id) in self.sounding.iter_mut().enumerate() {
            if *id != NOT_SOUNDING {
                self.out.push(0, EventKind::NoteOff { channel: (i / 128) as u8, key: (i % 128) as u8, velocity: 0.0, note_id: *id });
                *id = NOT_SOUNDING;
            }
        }
    }

    /// Note what the events about to be handed on start and end.
    fn track(&mut self, from: usize) {
        for e in &self.out.events[from..] {
            match e.kind {
                EventKind::NoteOn { channel, key, note_id, .. } => self.sounding[(channel as usize & 15) * 128 + (key as usize & 127)] = note_id,
                EventKind::NoteOff { channel, key, .. } => self.sounding[(channel as usize & 15) * 128 + (key as usize & 127)] = NOT_SOUNDING,
                EventKind::AllNotesOff => self.sounding.fill(NOT_SOUNDING),
                _ => {}
            }
        }
    }
}

/// Make every MIDI effect of the chain close what it left open and reset.
fn flush_midi(devs: &mut [DeviceNode]) {
    for d in devs {
        match d {
            DeviceNode::Proc(p) => {
                if let Some(m) = p.midi.as_deref_mut() {
                    m.flush = true;
                }
            }
            DeviceNode::Rack(r) => {
                for c in r.chains.iter_mut().chain(r.returns.iter_mut()) {
                    flush_midi(&mut c.devices);
                }
            }
            DeviceNode::Missing(_) => {}
        }
    }
}

/// Whether a MIDI effect of `old` that left notes open has no counterpart (by
/// id) in `new`: its notes can only be closed by an all-notes-off.
fn midi_lost(old: &[DeviceNode], new: &[DeviceNode]) -> bool {
    fn has(devs: &[DeviceNode], id: DeviceId) -> bool {
        devs.iter().any(|d| match d {
            DeviceNode::Proc(p) => p.id == id,
            DeviceNode::Rack(r) => r.chains.iter().chain(&r.returns).any(|c| has(&c.devices, id)),
            DeviceNode::Missing(_) => false,
        })
    }
    old.iter().any(|d| match d {
        DeviceNode::Proc(p) => p.midi.as_deref().is_some_and(|m| m.sounding.iter().any(|s| *s != NOT_SOUNDING)) && !has(new, p.id),
        DeviceNode::Rack(r) => r.chains.iter().chain(&r.returns).any(|c| midi_lost(&c.devices, new)),
        DeviceNode::Missing(_) => false,
    })
}

/// Buffers of [`ProcNode::chunk`].
pub(crate) struct ChunkScratch {
    pub buf: AudioBuffer,
    pub events: EventList,
}

impl ProcNode {
    /// Run a MIDI effect over the events `cur` reach it with. Returns `true`
    /// when the chain goes on with this device's output
    /// (`self.midi.out.events`) instead of `cur`: a disabled device, one
    /// that fell, or one with nothing to say passes `cur` on untouched.
    fn run_midi(&mut self, cur: &[Event], tr: &Transport, sr: f64, frames: usize, enabled: bool) -> bool {
        let Some(m) = self.midi.as_deref_mut() else { return false };
        if self.proc.is_crashed() {
            return false;
        }
        // SAFETY: the audio thread owns the installed graph (see `SharedProcessor`).
        let proc = unsafe { self.proc.get_mut() };
        m.out.clear();
        if !enabled {
            // Switched off: whatever it had started is closed once, then it
            // is transparent.
            if m.was_enabled {
                m.was_enabled = false;
                m.flush = true;
            }
            if !m.flush {
                return false;
            }
            m.close_open();
            m.flush = false;
            if guarded(|| proc.reset()).is_none() {
                self.proc.mark_crashed();
            }
            for e in cur {
                m.out.push(e.offset, e.kind);
            }
            return true;
        }
        m.was_enabled = true;
        if m.flush {
            m.close_open();
            m.flush = false;
            if guarded(|| proc.reset()).is_none() {
                self.proc.mark_crashed();
                return !m.out.events.is_empty();
            }
        }
        let from = m.out.events.len();
        let ctx = MidiContext { sample_rate: sr, frames, events: cur, transport: tr };
        let ran = guarded(|| proc.process_midi(&ctx, &mut EventSink::new(&mut m.out.events, &mut m.dropped)));
        if ran.is_none() {
            self.proc.mark_crashed();
            m.out.clear();
            return false;
        }
        m.track(from);
        m.out.sort();
        true
    }

    /// Process the window of `buf` in pieces cut at the `Param` events of
    /// `auto_events` (offsets in the whole block): each event is applied
    /// with `set_param` at its sample, so a jump lands where it is drawn.
    /// `events` are window-relative.
    fn run_chunked(&mut self, buf: &mut AudioBuffer, events: &[Event], tr: &Transport, sr: f64, side: &mut SideIn<'_>, win: (usize, usize)) {
        let Some(scratch) = self.chunk.as_deref_mut() else { return };
        // SAFETY: the audio thread owns the installed graph (see `SharedProcessor`).
        let proc = unsafe { self.proc.get_mut() };
        let n = buf.frames();
        let (a0, b0) = (win.0 as u32, win.1 as u32);
        let autos = &self.auto_events.events;
        let mut ei = autos.partition_point(|e| e.offset < a0);
        let side_base = side.offset;
        let mut a = 0usize;
        while a < n {
            while let Some(e) = autos.get(ei).filter(|e| e.offset < b0 && (e.offset - a0) as usize <= a) {
                if let EventKind::Param { id, value } = e.kind {
                    proc.set_param(id, value);
                }
                ei += 1;
            }
            let b = autos.get(ei).filter(|e| e.offset < b0).map_or(n, |e| (e.offset - a0) as usize).clamp(a + 1, n);
            let len = b - a;
            scratch.buf.set_frames(len);
            for ch in 0..buf.channels().min(scratch.buf.channels()) {
                scratch.buf.channel_mut(ch).copy_from_slice(&buf.channel(ch)[a..b]);
            }
            scratch.events.clear();
            for e in events.iter().filter(|e| (e.offset as usize) >= a && (e.offset as usize) < b) {
                scratch.events.push(e.offset - a as u32, e.kind);
            }
            let mut piece = *tr;
            piece.position_samples += a as i64;
            piece.position_beats += a as f64 * tr.bpm / (60.0 * sr);
            side.offset = side_base + a;
            let sidechain = self.sidechain.and_then(|slot| side.chunk(slot, len));
            let mut ctx = ProcessContext { sample_rate: sr, buffer: &mut scratch.buf, sidechain, events: &scratch.events.events, transport: &piece };
            proc.process(&mut ctx);
            for ch in 0..buf.channels().min(scratch.buf.channels()) {
                buf.channel_mut(ch)[a..b].copy_from_slice(scratch.buf.channel(ch));
            }
            a = b;
        }
        side.offset = side_base;
        self.proc.publish_latency(proc.latency());
    }

    /// The processor panicked: it leaves the signal path for good (shared
    /// with every graph that holds it) and `buf` — what it was to process —
    /// is silenced.
    fn fall(&mut self, buf: &mut AudioBuffer) {
        self.proc.mark_crashed();
        self.sleeping = false;
        buf.clear();
    }
}

/// The sidechain signals available to the devices of one track.
pub(crate) struct SideIn<'a> {
    /// One buffer per source track: the block being processed when the
    /// source comes earlier in the graph, otherwise the previous one.
    pub bufs: &'a [AudioBuffer],
    /// The frames of the current chunk, copied out for the device.
    pub scratch: &'a mut AudioBuffer,
    /// Where the current chunk starts inside the segment.
    pub offset: usize,
}

impl SideIn<'_> {
    /// The chunk of sidechain `slot` that lines up with a device buffer of
    /// `frames` frames, or `None` when this track has no sidechain scratch.
    fn chunk(&mut self, slot: usize, frames: usize) -> Option<&AudioBuffer> {
        let src = self.bufs.get(slot)?;
        if self.scratch.capacity() < frames || self.scratch.channels() < 2 {
            return None;
        }
        self.scratch.set_frames(frames);
        for ch in 0..2 {
            let from = src.channel(ch.min(src.channels().saturating_sub(1)));
            let to = self.scratch.channel_mut(ch);
            let have = from.len().saturating_sub(self.offset).min(frames);
            to[..have].copy_from_slice(&from[self.offset..self.offset + have]);
            to[have..].fill(0.0);
        }
        Some(&*self.scratch)
    }
}

pub(crate) struct RackNode {
    pub id: DeviceId,
    pub kind: RackKind,
    pub enabled: Arc<AtomicBool>,
    pub chains: Vec<ChainNode>,
    /// The Drum Rack's internal return buses: effect chains the pads send to.
    /// They reuse [`ChainNode`] (devices, buffer, gain, delay).
    pub returns: Vec<ChainNode>,
    /// Chain Select value (0..=127) the chains' selector zones are read at.
    pub selector: u8,
    /// Holds the dry path back by the slowest return's latency, so a pad and
    /// its reverb line up.
    pub dry_delay: DelayLine,
    pub input: AudioBuffer,
}

pub(crate) struct ChainNode {
    pub devices: Vec<DeviceNode>,
    /// Volume × pan, zero when muted or silenced by a chain solo.
    pub gain: (f32, f32),
    /// Drum Rack: the note that hits the pad (Receive); other racks use the key zone.
    pub note: Option<u8>,
    /// Drum Rack: the note the pad's devices get (Play).
    pub play_note: u8,
    pub key_range: (u8, u8),
    pub velocity_range: (u8, u8),
    pub selector_range: (u8, u8),
    /// Fade widths outside the low and high end of each zone.
    pub key_fade: (u8, u8),
    pub vel_fade: (u8, u8),
    pub sel_fade: (u8, u8),
    /// Choke group (0 = none).
    pub choke: u8,
    /// Linear send level into each return (0 = off).
    pub sends: Vec<f32>,
    /// The return this chain's output goes to instead of the rack output.
    pub to_return: Option<usize>,
    pub buf: AudioBuffer,
    pub events: EventList,
    /// Aligns chains of different latency before they are summed.
    pub delay: DelayLine,
    // ── Runtime state: carried over by `adopt`.
    /// Gain applied at the end of the last block; the next one ramps from it.
    pub cur_gain: [f32; 2],
    /// `cur_gain` is a real gain (the first block of a graph without
    /// predecessor starts at the target).
    pub primed: bool,
    /// Keys received (bit per key) and the key each was played as, so a
    /// note-off finds its note whatever was edited meanwhile.
    pub held: [u64; 2],
    pub played: [u8; 128],
    /// Choke fade (1 = open) and whether it is running.
    pub choke_gain: f32,
    pub choking: bool,
    /// Offset in the block where a choke starts (`u32::MAX` = none).
    pub choke_at: u32,
    /// Send `AllNotesOff` to the devices at the start of the next block.
    pub all_off: bool,
    /// The chain's gain is 0 and it has faded out: it is not processed.
    pub asleep: bool,
    /// This block's target gain (volume × pan × selector zone).
    pub target: (f32, f32),
}

impl ChainNode {
    fn holds(&self, key: u8) -> bool {
        self.held[(key >> 6) as usize] >> (key & 63) & 1 == 1
    }

    fn set_held(&mut self, key: u8, on: bool) {
        let bit = 1u64 << (key & 63);
        if on {
            self.held[(key >> 6) as usize] |= bit;
        } else {
            self.held[(key >> 6) as usize] &= !bit;
        }
    }
}

/// Gain 0..=1 of a zone `range` with fades of `fade.0` below and `fade.1`
/// above it: full inside, a linear ramp outside over the fade width, 0 beyond.
pub(crate) fn fade_gain(x: f32, range: (u8, u8), fade: (u8, u8)) -> f32 {
    let (lo, hi) = (range.0.min(range.1) as f32, range.0.max(range.1) as f32);
    if x >= lo && x <= hi {
        1.0
    } else if x < lo {
        if fade.0 == 0 { 0.0 } else { ((x - (lo - fade.0 as f32)) / fade.0 as f32).clamp(0.0, 1.0) }
    } else if fade.1 == 0 {
        0.0
    } else {
        (((hi + fade.1 as f32) - x) / fade.1 as f32).clamp(0.0, 1.0)
    }
}

impl DeviceNode {
    pub fn id(&self) -> DeviceId {
        match self {
            Self::Proc(p) => p.id,
            Self::Rack(r) => r.id,
            Self::Missing(id) => *id,
        }
    }
}

const SILENCE_THRESH: f32 = 1e-7; // −140 dBFS

#[inline]
fn buffer_silent(buf: &AudioBuffer) -> bool {
    for ch in 0..buf.channels() {
        if buf.channel(ch).iter().any(|s| s.abs() >= SILENCE_THRESH) {
            return false;
        }
    }
    true
}

/// Run a chain over `buf`, the frames `win.0..win.1` of the block: `events`
/// are already relative to the window; a device's own timed automation
/// (block offsets) is cut to the window and merged in, in offset order.
#[allow(clippy::too_many_arguments)]
fn process_devices<'a>(devs: &'a mut [DeviceNode], buf: &mut AudioBuffer, events: &'a [Event], tr: &Transport, sr: f64, side: &mut SideIn<'_>, win: (usize, usize), allow_sleep: bool) {
    // The events the next device sees: the chain's own until a MIDI effect
    // replaces them with its output.
    let mut cur: &'a [Event] = events;
    for d in devs {
        match d {
            DeviceNode::Proc(p) => {
                if p.is_midi {
                    let enabled = p.enabled.load(Ordering::Relaxed);
                    if p.run_midi(cur, tr, sr, buf.frames(), enabled) {
                        let p: &'a ProcNode = p;
                        if let Some(m) = p.midi.as_deref() {
                            cur = &m.out.events;
                        }
                    }
                    continue;
                }
                if !p.enabled.load(Ordering::Relaxed) {
                    continue;
                }
                if p.proc.is_crashed() {
                    // Fell in an earlier block (or in an earlier graph): silent.
                    buf.clear();
                    continue;
                }
                let frames = buf.frames();
                if p.chunk.is_some() && p.auto_events.events.iter().any(|e| (e.offset as usize) >= win.0 && (e.offset as usize) < win.1) {
                    // A stock device with automation moving in this window.
                    p.proc.take_wake();
                    p.sleeping = false;
                    p.silent_samples = 0;
                    if p.is_instrument {
                        buf.clear();
                    }
                    if guarded(|| p.run_chunked(buf, cur, tr, sr, side, win)).is_none() {
                        p.fall(buf);
                    }
                    continue;
                }
                // SAFETY: the audio thread owns the installed graph and is the
                // only user of its processors (see `SharedProcessor`).
                let proc = unsafe { p.proc.get_mut() };
                if p.is_instrument {
                    buf.clear();
                }
                let sidechain = p.sidechain.and_then(|slot| side.chunk(slot, frames));
                let merged_events = if p.auto_events.events.is_empty() || p.chunk.is_some() {
                    cur
                } else {
                    merge_window(&mut p.merged, cur, &p.auto_events.events, win);
                    &p.merged.events
                };
                let input_silent = p.is_instrument || buffer_silent(buf);
                let side_silent = sidechain.as_ref().is_none_or(|sc| buffer_silent(sc));
                let woken = p.proc.take_wake();
                let idle_in = allow_sleep && !woken && merged_events.is_empty() && input_silent && side_silent;
                if p.sleeping && idle_in {
                    continue;
                }
                p.sleeping = false;
                let shared = &p.proc;
                let ran = guarded(|| {
                    let mut ctx = ProcessContext { sample_rate: sr, buffer: &mut *buf, sidechain, events: merged_events, transport: tr };
                    proc.process(&mut ctx);
                    shared.publish_latency(proc.latency());
                });
                if ran.is_none() {
                    p.fall(buf);
                    continue;
                }
                if idle_in && buffer_silent(buf) {
                    let min_guard = if p.is_instrument { (sr * 0.5) as u32 } else { (sr * 0.25) as u32 };
                    let Some(tail) = guarded(|| proc.tail()) else {
                        p.fall(buf);
                        continue;
                    };
                    let needed = tail.max(min_guard);
                    p.silent_samples = p.silent_samples.saturating_add(frames as u32);
                    if p.silent_samples >= needed {
                        p.sleeping = true;
                    }
                } else {
                    p.silent_samples = 0;
                }
            }
            DeviceNode::Rack(r) => {
                if r.enabled.load(Ordering::Relaxed) {
                    r.process(buf, cur, tr, sr, side, win, allow_sleep);
                }
            }
            DeviceNode::Missing(_) => {}
        }
    }
}

/// `out` = `events` (window offsets) and the part of `timed` (block offsets)
/// inside `win`, rebased, both kept in offset order (at equal offsets the
/// block's own events come first). Real-time safe: `out` has fixed capacity.
fn merge_window(out: &mut EventList, events: &[Event], timed: &[Event], win: (usize, usize)) {
    out.clear();
    let (a, b) = (win.0 as u32, win.1 as u32);
    let mut timed = timed.iter().filter(|e| e.offset >= a && e.offset < b).map(|e| Event { offset: e.offset - a, kind: e.kind }).peekable();
    for e in events {
        while let Some(t) = timed.next_if(|t| t.offset < e.offset) {
            out.push(t.offset, t.kind);
        }
        out.push(e.offset, e.kind);
    }
    for t in timed {
        out.push(t.offset, t.kind);
    }
}

/// The processor node of `id` in a chain, at any rack depth.
pub(crate) fn find_proc_mut(devs: &mut [DeviceNode], id: DeviceId) -> Option<&mut ProcNode> {
    for d in devs {
        match d {
            DeviceNode::Proc(p) if p.id == id => return Some(p),
            DeviceNode::Rack(r) => {
                for c in r.chains.iter_mut().chain(r.returns.iter_mut()) {
                    if let Some(p) = find_proc_mut(&mut c.devices, id) {
                        return Some(p);
                    }
                }
            }
            _ => {}
        }
    }
    None
}

fn in_range(v: u8, r: (u8, u8)) -> bool {
    v >= r.0.min(r.1) && v <= r.0.max(r.1)
}

/// Choke ramp length.
const CHOKE_SECONDS: f64 = 0.005;

impl RackNode {
    /// Route one event to a chain: the events it gets are appended to its
    /// list. Returns the chain's choke group when the event was a note-on it
    /// took.
    fn route(kind: RackKind, c: &mut ChainNode, e: &Event) -> Option<u8> {
        let drum = kind == RackKind::Drum;
        let hit = |key: u8| match (drum, c.note) {
            (true, Some(n)) => key == n,
            _ => in_range(key, c.key_range),
        };
        let play = |key: u8| if drum { c.play_note } else { key };
        match e.kind {
            EventKind::NoteOn { channel, key, velocity, note_id } => {
                let v = if drum {
                    hit(key).then_some(velocity)
                } else {
                    // Fades turn the zone edges into a velocity ramp.
                    let g = fade_gain(key as f32, c.key_range, c.key_fade) * fade_gain((velocity * 127.0).round(), c.velocity_range, c.vel_fade);
                    (g > 0.0 && velocity * g > 0.0).then_some(velocity * g)
                }?;
                let out = play(key);
                c.set_held(key, true);
                c.played[key as usize & 127] = out;
                c.events.push(e.offset, EventKind::NoteOn { channel, key: out, velocity: v, note_id });
                // A hit reopens a choked pad.
                c.choke_gain = 1.0;
                c.choking = false;
                c.choke_at = u32::MAX;
                Some(c.choke)
            }
            EventKind::NoteOff { channel, key, velocity, note_id } => {
                if c.holds(key) {
                    c.set_held(key, false);
                    c.events.push(e.offset, EventKind::NoteOff { channel, key: c.played[key as usize & 127], velocity, note_id });
                } else if hit(key) {
                    c.events.push(e.offset, EventKind::NoteOff { channel, key: play(key), velocity, note_id });
                }
                None
            }
            EventKind::NoteExpression { key, .. } => {
                // (Follows its note: held keys only.)
                if c.holds(key) {
                    let mut kind = e.kind;
                    if let EventKind::NoteExpression { key: k, .. } = &mut kind {
                        *k = c.played[key as usize & 127];
                    }
                    c.events.push(e.offset, kind);
                }
                None
            }
            EventKind::AllNotesOff => {
                c.held = [0; 2];
                c.events.push(e.offset, e.kind);
                None
            }
            _ => {
                c.events.push(e.offset, e.kind);
                None
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn process(&mut self, buf: &mut AudioBuffer, events: &[Event], tr: &Transport, sr: f64, side: &mut SideIn<'_>, win: (usize, usize), allow_sleep: bool) {
        let n = buf.frames();
        self.input.set_frames(n);
        self.input.copy_from(buf);
        buf.clear();
        let kind = self.kind;
        let selector = self.selector as f32;
        for r in &mut self.returns {
            r.buf.set_frames(n);
            r.buf.clear();
        }
        // This block's target gain of each chain, and who is awake.
        for c in &mut self.chains {
            let sel = if kind == RackKind::Drum { 1.0 } else { fade_gain(selector, c.selector_range, c.sel_fade) };
            c.target = (c.gain.0 * sel, c.gain.1 * sel);
            c.events.clear();
            c.choke_at = u32::MAX;
            if c.asleep {
                if c.target == (0.0, 0.0) {
                    continue;
                }
                c.wake();
            }
            if std::mem::take(&mut c.all_off) {
                c.held = [0; 2];
                c.events.push(0, EventKind::AllNotesOff);
            }
        }
        for e in events {
            for ci in 0..self.chains.len() {
                if self.chains[ci].asleep {
                    continue;
                }
                if let Some(group) = Self::route(kind, &mut self.chains[ci], e)
                    && group != 0
                {
                    // The pad hit cuts the others of its group, from this sample.
                    for (di, d) in self.chains.iter_mut().enumerate() {
                        if di != ci && d.choke == group {
                            d.choke_at = d.choke_at.min(e.offset);
                        }
                    }
                }
            }
        }
        let choke_step = (1.0 / (CHOKE_SECONDS * sr)) as f32;
        for c in &mut self.chains {
            if c.asleep {
                continue;
            }
            if !c.primed {
                c.cur_gain = [c.target.0, c.target.1];
                c.primed = true;
            }
            c.buf.set_frames(n);
            c.buf.copy_from(&self.input);
            process_devices(&mut c.devices, &mut c.buf, &c.events.events, tr, sr, side, win, allow_sleep);
            c.delay.process(&mut c.buf);
            let was_silent = c.cur_gain == [0.0, 0.0];
            c.apply_gain(choke_step);
            if c.target == (0.0, 0.0) && was_silent && !c.choking {
                // Already faded out and still at 0: stop processing it until
                // its gain comes back (a block that is still ramping down
                // is summed first).
                c.asleep = true;
                c.all_off = true;
                continue;
            }
            for (j, &send) in c.sends.iter().enumerate() {
                // (A chain whose output is a return does not also send to it.)
                if send > 0.0
                    && c.to_return != Some(j)
                    && let Some(r) = self.returns.get_mut(j)
                {
                    r.buf.add_from(&c.buf, send);
                }
            }
            match c.to_return.and_then(|j| self.returns.get_mut(j)) {
                Some(r) => r.buf.add_from(&c.buf, 1.0),
                None => {
                    if buf.channels() >= 2 && c.buf.channels() >= 2 {
                        buf.add_from(&c.buf, 1.0);
                    }
                }
            }
        }
        if self.returns.is_empty() {
            return;
        }
        // The returns: dry path held back by the slowest, then each return's
        // effects over what was sent to it.
        self.dry_delay.process(buf);
        for r in &mut self.returns {
            r.target = r.gain;
            if !r.primed {
                r.cur_gain = [r.target.0, r.target.1];
                r.primed = true;
            }
            process_devices(&mut r.devices, &mut r.buf, &[], tr, sr, side, win, allow_sleep);
            r.delay.process(&mut r.buf);
            r.apply_gain(choke_step);
            buf.add_from(&r.buf, 1.0);
        }
    }
}

/// Forget the state of every device of a chain: nothing of what was in
/// flight when the chain went to sleep may come out when it wakes.
fn reset_devices(devs: &mut [DeviceNode]) {
    for d in devs {
        match d {
            DeviceNode::Proc(p) => {
                p.sleeping = false;
                p.silent_samples = 0;
                if !p.proc.is_crashed() {
                    // SAFETY: the audio thread owns the installed graph (see `SharedProcessor`).
                    let proc = unsafe { p.proc.get_mut() };
                    if guarded(|| proc.reset()).is_none() {
                        p.proc.mark_crashed();
                    }
                }
            }
            DeviceNode::Rack(r) => {
                r.dry_delay.clear();
                for c in r.chains.iter_mut().chain(r.returns.iter_mut()) {
                    c.delay.clear();
                    reset_devices(&mut c.devices);
                }
            }
            DeviceNode::Missing(_) => {}
        }
    }
}

impl ChainNode {
    /// Back from sleep: the delay line and the devices start empty, so the
    /// audio (and the reverb tails) from before it fell asleep are not
    /// replayed.
    fn wake(&mut self) {
        self.asleep = false;
        self.delay.clear();
        reset_devices(&mut self.devices);
    }

    /// Ramp the chain's gain from where the last block left it to this
    /// block's target, with the choke fade on top, in place on `buf`.
    fn apply_gain(&mut self, choke_step: f32) {
        let n = self.buf.frames();
        if n == 0 || self.buf.channels() < 2 {
            return;
        }
        let ([cl, cr], (tl, tr)) = (self.cur_gain, self.target);
        let (sl, sr_) = ((tl - cl) / n as f32, (tr - cr) / n as f32);
        let (mut gl, mut gr) = (cl, cr);
        let (bl, br) = self.buf.stereo_mut();
        for i in 0..n {
            if i as u32 == self.choke_at {
                self.choking = true;
            }
            if self.choking {
                self.choke_gain -= choke_step;
                if self.choke_gain <= 0.0 {
                    // Silent: the notes are cut at the start of the next block.
                    self.choke_gain = 0.0;
                    self.choking = false;
                    self.all_off = true;
                }
            }
            gl += sl;
            gr += sr_;
            bl[i] *= gl * self.choke_gain;
            br[i] *= gr * self.choke_gain;
        }
        self.cur_gain = [tl, tr];
    }
}

pub(crate) fn for_each_proc(devs: &[DeviceNode], f: &mut impl FnMut(&ProcNode)) {
    for d in devs {
        match d {
            DeviceNode::Proc(p) => f(p),
            DeviceNode::Rack(r) => {
                for c in r.chains.iter().chain(&r.returns) {
                    for_each_proc(&c.devices, f);
                }
            }
            DeviceNode::Missing(_) => {}
        }
    }
}

fn adopt_devices(new: &mut [DeviceNode], old: &mut [DeviceNode]) {
    for (i, n) in new.iter_mut().enumerate() {
        let id = n.id();
        match (n, old.get_mut(i).filter(|o| o.id() == id)) {
            (DeviceNode::Proc(np), Some(DeviceNode::Proc(op))) => {
                np.silent_samples = op.silent_samples;
                np.sleeping = op.sleeping;
                // The notes a MIDI effect has open are closed by the new
                // node's first block (its `flush` is set at compile).
                if let (Some(nm), Some(om)) = (np.midi.as_deref_mut(), op.midi.as_deref()) {
                    nm.sounding.copy_from_slice(&om.sounding);
                    nm.was_enabled = om.was_enabled;
                }
            }
            (DeviceNode::Rack(nr), Some(DeviceNode::Rack(or))) => {
                for (nc, oc) in nr.chains.iter_mut().zip(or.chains.iter_mut()) {
                    adopt_chain(nc, oc);
                }
                // A chain with no counterpart fades in from silence.
                for nc in nr.chains.iter_mut().skip(or.chains.len()) {
                    nc.primed = true;
                }
                for (nc, oc) in nr.returns.iter_mut().zip(or.returns.iter_mut()) {
                    adopt_chain(nc, oc);
                }
                for nc in nr.returns.iter_mut().skip(or.returns.len()) {
                    nc.primed = true;
                }
                nr.dry_delay.adopt(&mut or.dry_delay);
            }
            // A rack that was not there before: its chains come in from silence.
            (DeviceNode::Rack(nr), _) => {
                for c in nr.chains.iter_mut().chain(nr.returns.iter_mut()) {
                    c.primed = true;
                }
            }
            _ => {}
        }
    }
}

/// Move a chain's runtime state over from the one it replaces.
fn adopt_chain(nc: &mut ChainNode, oc: &mut ChainNode) {
    nc.delay.adopt(&mut oc.delay);
    nc.cur_gain = oc.cur_gain;
    nc.primed = oc.primed;
    nc.held = oc.held;
    nc.played = oc.played;
    nc.choke_gain = oc.choke_gain;
    nc.choking = oc.choking;
    nc.all_off = oc.all_off;
    nc.asleep = oc.asleep;
    adopt_devices(&mut nc.devices, &mut oc.devices);
}

// ───────────────────────────── Automation ─────────────────────────────

pub(crate) enum ParamTarget {
    /// A device parameter, hosted or stock: `Param` events at sample offsets
    /// inside the block, written into the device node's `auto_events`. A
    /// hosted plugin gets them as VST3 `IParameterChanges` points / CLAP
    /// param-value events with their time; a stock device is processed in
    /// pieces cut at them ([`ProcNode::run_chunked`]). Either way the rest
    /// of the chain keeps whole blocks.
    Timed(DeviceId, u32),
    /// Macro: `(device, param, min, max)` for every mapping, each a `Timed`
    /// target scaled into its range.
    TimedMacro(Vec<(DeviceId, u32, f32, f32)>),
    /// A device's on/off switch: applied every [`CONTROL_BLOCK`] frames,
    /// which cuts the track's chain into control blocks.
    Enabled(Arc<AtomicBool>),
}

pub(crate) struct ParamAuto {
    pub lane: AutomationLane,
    pub target: ParamTarget,
    pub last: f32,
}

/// This block's values of `lane` as `(offset, value)`: one every
/// [`CONTROL_BLOCK`] frames where the value moved plus one on the exact
/// sample of every breakpoint inside the block (so a jump lands where it is
/// drawn). `start` is the block's first sample on the timeline the lane is
/// read on. Real-time safe.
fn emit_values(lane: &AutomationLane, last: &mut f32, ctx: &TimeCtx<'_>, start: i64, n: usize, sink: &mut impl FnMut(u32, f32)) {
    if !lane.enabled || lane.points.is_empty() || n == 0 {
        return;
    }
    let (t0, t1) = (ctx.tick_of(start), ctx.tick_of(start + n as i64));
    // Breakpoints inside the block, as block offsets (ascending).
    let pts = &lane.points;
    let mut k = pts.partition_point(|p| (p.tick as f64) <= t0);
    let next_break = |k: &mut usize| -> Option<(usize, Tick)> {
        while let Some(p) = pts.get(*k) {
            if p.tick as f64 >= t1 {
                return None;
            }
            *k += 1;
            let off = ctx.sample_of(p.tick) - start;
            if off > 0 && (off as usize) < n {
                return Some((off as usize, p.tick));
            }
        }
        None
    };
    let mut brk = next_break(&mut k);
    let mut a = 0;
    loop {
        // The next point in time: a control step or a breakpoint, whichever
        // comes first (the control step on a tie).
        let (off, value) = match brk {
            Some((b, tick)) if a >= n || b < a => {
                brk = next_break(&mut k);
                (b, lane.value_at(tick))
            }
            _ if a < n => {
                let v = lane_value(lane, ctx.tick_of(start + a as i64));
                a += CONTROL_BLOCK;
                (a - CONTROL_BLOCK, v)
            }
            _ => break,
        };
        let Some(v) = value else { continue };
        if (v - *last).abs() < 1e-6 {
            continue;
        }
        *last = v;
        sink(off as u32, v);
    }
}

impl ParamAuto {
    /// True when this lane is applied through timed events.
    pub(crate) fn is_timed(&self) -> bool {
        !matches!(self.target, ParamTarget::Enabled(_))
    }

    /// The devices whose `auto_events` this lane writes.
    fn for_each_device(&self, mut f: impl FnMut(DeviceId)) {
        match &self.target {
            ParamTarget::Timed(d, _) => f(*d),
            ParamTarget::TimedMacro(maps) => maps.iter().for_each(|m| f(m.0)),
            ParamTarget::Enabled(_) => {}
        }
    }

    /// Write this block's values into the `auto_events` of its devices.
    fn emit(&mut self, devices: &mut [DeviceNode], ctx: &TimeCtx<'_>, start: i64, n: usize) {
        let Self { lane, target, last } = self;
        match target {
            ParamTarget::Timed(device, id) => {
                if let Some(node) = find_proc_mut(devices, *device) {
                    emit_values(lane, last, ctx, start, n, &mut |off, value| node.auto_events.push(off, EventKind::Param { id: *id, value }));
                }
            }
            ParamTarget::TimedMacro(maps) => {
                emit_values(lane, last, ctx, start, n, &mut |off, v| {
                    for &(device, id, min, max) in maps.iter() {
                        if let Some(node) = find_proc_mut(devices, device) {
                            node.auto_events.push(off, EventKind::Param { id, value: min + (max - min) * v });
                        }
                    }
                });
            }
            ParamTarget::Enabled(_) => {}
        }
    }

    /// Apply a switch lane at `tick` (the other kinds go through events).
    fn apply(&mut self, tick: f64) {
        let ParamTarget::Enabled(flag) = &self.target else { return };
        let Some(v) = lane_value(&self.lane, tick) else { return };
        if (v - self.last).abs() < 1e-6 {
            return;
        }
        self.last = v;
        flag.store(v >= 0.5, Ordering::Relaxed);
    }
}

/// `set_param` from the audio thread: wakes the processor, skips one that
/// fell, and takes one that panics out of the signal path.
fn set_param_guarded(p: &SharedProcessor, id: u32, v: f32) {
    if p.is_crashed() {
        return;
    }
    p.wake();
    // SAFETY: audio thread, see `SharedProcessor`.
    let proc = unsafe { p.get_mut() };
    if guarded(|| proc.set_param(id, v)).is_none() {
        p.mark_crashed();
    }
}

// ───────────────────────────── Tracks ─────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OutputTarget {
    Track(usize),
    /// The hardware output bus (Main, or a track routed straight to hardware).
    Out,
    None,
}

pub(crate) struct SendNode {
    /// Position in the track's `sends` list in the project.
    pub index: u16,
    pub target: usize,
    /// The receiving track, to tell a send that continues from one that was
    /// redirected when a graph replaces another.
    pub target_id: TrackId,
    pub level_db: f32,
    pub pre_fader: bool,
    pub lane: Option<AutomationLane>,
    pub cur: f32,
    /// `cur` holds a real gain: false in a graph that has not run yet, whose
    /// first block starts at the target instead of fading in from silence.
    pub primed: bool,
    pub delay: DelayLine,
    pub buf: AudioBuffer,
}

pub(crate) struct ArrPlacement {
    pub clip: usize,
    pub start: Tick,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Queued {
    /// `None` = stop.
    pub scene: Option<usize>,
    pub at: Tick,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SessionRt {
    pub playing: Option<(usize, Tick)>,
    pub queued: Option<Queued>,
    /// A session clip took over the track; the arrangement stays silent
    /// until "back to arrangement".
    pub override_arrangement: bool,
}

#[derive(Default)]
pub(crate) struct MeterAcc {
    pub peak: [f32; 2],
    pub sumsq: [f64; 2],
    pub count: usize,
    pub rms: [f32; 2],
}

impl MeterAcc {
    fn add(&mut self, buf: &AudioBuffer) {
        for ch in 0..2.min(buf.channels()) {
            for s in buf.channel(ch) {
                let a = s.abs();
                if a > self.peak[ch] {
                    self.peak[ch] = a;
                }
                self.sumsq[ch] += (*s as f64) * (*s as f64);
            }
        }
        self.count += buf.frames();
    }

    /// Fold into the ~300 ms RMS and return `(peak, rms)`, resetting the block.
    fn finish(&mut self, sr: f64) -> ([f32; 2], [f32; 2]) {
        let peak = self.peak;
        if self.count > 0 {
            let a = (-(self.count as f64) / (0.3 * sr)).exp();
            for ch in 0..2 {
                let ms = self.sumsq[ch] / self.count as f64;
                let prev = self.rms[ch] as f64;
                self.rms[ch] = (prev * prev * a + ms * (1.0 - a)).sqrt() as f32;
            }
        }
        self.peak = [0.0; 2];
        self.sumsq = [0.0; 2];
        self.count = 0;
        (peak, self.rms)
    }
}

pub(crate) struct TrackNode {
    pub id: TrackId,
    pub kind: TrackKind,
    pub arm: bool,
    pub monitor: Monitor,
    /// Device input channels `(first, count)` this audio track records and
    /// monitors; `None` = the engine's default input.
    pub input_sel: Option<(u16, u16)>,
    /// The track has no input: it neither records nor monitors.
    pub no_input: bool,
    /// The track's own stereo view of the device input for one segment.
    pub in_buf: AudioBuffer,
    pub has_input: bool,
    pub devices: Vec<DeviceNode>,
    pub param_autos: Vec<ParamAuto>,
    pub buf: AudioBuffer,
    pub chunk_buf: AudioBuffer,
    pub chunk_events: EventList,
    pub clip_buf: AudioBuffer,
    pub clip_delay: DelayLine,
    pub clips: Vec<CompiledClip>,
    pub arrangement: Vec<ArrPlacement>,
    /// Graph scene index → clip index.
    pub slots: Vec<Option<usize>>,
    pub session_index: Option<usize>,
    pub session: SessionRt,
    pub events: EventList,
    pub live: EventList,
    pub active: ActiveNotes,
    pub rng: Rng,
    pub release_all: bool,
    /// Send an all-notes-off down the chain at the next segment (a MIDI
    /// effect that held notes is gone from the new graph).
    pub panic_notes: bool,
    // Mixer.
    pub volume_db: f32,
    pub pan: f32,
    pub mute: bool,
    pub silenced: bool,
    pub vca: Option<usize>,
    pub volume_lane: Option<AutomationLane>,
    pub pan_lane: Option<AutomationLane>,
    pub mute_lane: Option<AutomationLane>,
    /// For VCA tracks: `(dB at segment start, dB at end, muted)`.
    pub vca_out: (f32, f32, bool),
    pub vca_in: (f32, f32, bool),
    pub cur_gain: [f32; 2],
    /// `cur_gain` holds a real gain: false until the track's first block
    /// (the graph is new), which starts at the fader's target. A graph that
    /// replaces another inherits both from the track it replaces.
    pub gain_primed: bool,
    pub sends: Vec<SendNode>,
    pub output: OutputTarget,
    pub out_delay: DelayLine,
    /// Signal at the fader lags musical time by this many samples.
    pub fader_latency: u32,
    /// Signal entering the chain lags musical time by this many samples.
    pub chain_latency_in: u32,
    pub meter: Option<usize>,
    pub meter_acc: MeterAcc,
    /// Offline stem capture (post-fader, aligned with the output bus).
    pub tap: Option<Box<TapNode>>,
    /// The UI's analysis ring, fed while it asks for this track.
    pub analysis: Option<Arc<AnalysisTap>>,
    /// Where this track's post-device signal is published for devices on
    /// other tracks that sidechain from it.
    pub side_out: Option<usize>,
    /// Scratch for the sidechain chunk handed to this track's devices
    /// (empty when none of them has a sidechain source).
    pub side_scratch: AudioBuffer,
}

/// Captures a track's post-fader signal for stem export, delayed so it lines
/// up with the output bus (`total latency − track output latency`).
pub(crate) struct TapNode {
    pub delay: DelayLine,
    pub scratch: AudioBuffer,
    /// The current block, written at each segment's output offset.
    pub block: AudioBuffer,
}

impl TrackNode {
    fn carries_audio(&self) -> bool {
        self.kind.carries_audio()
    }

    /// Fader gains at a tick.
    fn fader_target(&self, tick: f64, vca_db: f32, vca_mute: bool) -> (f32, f32) {
        let vol = self.volume_lane.as_ref().and_then(|l| lane_value(l, tick)).map_or(self.volume_db, volume_db_from_normalized);
        let pan = self.pan_lane.as_ref().and_then(|l| lane_value(l, tick)).map_or(self.pan, pan_from_normalized);
        let lane_mute = self.mute_lane.as_ref().and_then(|l| lane_value(l, tick)).is_some_and(|v| v >= 0.5);
        if self.mute || lane_mute || vca_mute || self.silenced {
            return (0.0, 0.0);
        }
        let g = db_to_gain(vol + vca_db);
        let (l, r) = pan_gains(pan);
        (l * g, r * g)
    }

    fn is_muted_at(&self, tick: f64, vca_mute: bool) -> bool {
        self.mute
            || self.silenced
            || vca_mute
            || self.mute_lane.as_ref().and_then(|l| lane_value(l, tick)).is_some_and(|v| v >= 0.5)
    }
}

/// Per-segment I/O for [`CompiledGraph::process_segment`].
pub(crate) struct SegmentIo<'a> {
    pub seg_start: i64,
    pub frames: usize,
    pub playing: bool,
    pub recording: bool,
    /// Live input (planar, stereo) for monitoring, with the segment offset.
    pub input: Option<(&'a AudioBuffer, usize)>,
    /// The whole device input of the block, interleaved, for tracks that
    /// pick their own channels (the block's frame 0 is `out_offset`'s base).
    pub raw: Option<(&'a [f32], usize)>,
    pub out: &'a mut AudioBuffer,
    pub out_offset: usize,
    pub loop_range: Option<(Tick, Tick)>,
}

pub struct CompiledGraph {
    pub(crate) sample_rate: f64,
    pub(crate) max_frames: usize,
    pub(crate) tempo: TempoMap,
    pub(crate) tracks: Vec<TrackNode>,
    pub(crate) inputs: Vec<AudioBuffer>,
    /// Sidechain buffers: the post-device, pre-fader signal of every track
    /// some device listens to.
    pub(crate) side: Vec<AudioBuffer>,
    pub(crate) out_bus: AudioBuffer,
    pub(crate) out_acc: MeterAcc,
    /// Sorted `(id, index)` for lookups on the audio thread.
    pub(crate) track_lookup: Vec<(TrackId, usize)>,
    pub(crate) scenes: Vec<SceneId>,
    pub(crate) scene_lookup: Vec<(SceneId, usize)>,
    pub(crate) total_latency: u32,
    pub(crate) meters: Arc<MeterBank>,
    pub(crate) session_status: Arc<SessionStatus>,
    pub(crate) warnings: Vec<String>,
    /// Chain latency per track (what `Track::reported_latency` should show).
    pub(crate) track_latency: Vec<(TrackId, u32)>,
    /// Processors that fell in this graph and were not yet reported.
    pub(crate) new_crashes: u32,
    /// Non-finite output samples repaired and not yet reported.
    pub(crate) sanitized: u64,
    /// Events the MIDI effects lost to full queues, as of the last block.
    pub(crate) midi_dropped: u64,
    /// Which compile made this graph (set by the control thread); the audio
    /// thread publishes the one it runs so the control thread can tell
    /// whether the newest graph is the installed one.
    pub(crate) generation: u64,
}

/// Replace NaN and infinities in `buf` by zero; returns how many.
/// Allocation-free, a single pass.
fn sanitize(buf: &mut AudioBuffer) -> u64 {
    let mut bad = 0u64;
    for ch in 0..buf.channels() {
        for s in buf.channel_mut(ch) {
            if !s.is_finite() {
                *s = 0.0;
                bad += 1;
            }
        }
    }
    bad
}

/// Count the processors that panicked and were not counted yet.
fn drain_crashes(devs: &[DeviceNode]) -> u32 {
    let mut n = 0;
    for d in devs {
        match d {
            DeviceNode::Proc(p) => n += p.proc.take_unreported_crash() as u32,
            DeviceNode::Rack(r) => {
                for c in r.chains.iter().chain(&r.returns) {
                    n += drain_crashes(&c.devices);
                }
            }
            DeviceNode::Missing(_) => {}
        }
    }
    n
}

impl CompiledGraph {
    pub fn max_frames(&self) -> usize {
        self.max_frames
    }

    pub fn total_latency(&self) -> u32 {
        self.total_latency
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Faults survived since the last call: `(processors that fell,
    /// samples repaired)`. The audio thread folds them into the status.
    pub(crate) fn take_faults(&mut self) -> (u32, u64) {
        (std::mem::take(&mut self.new_crashes), std::mem::take(&mut self.sanitized))
    }

    pub fn track_count(&self) -> usize {
        self.tracks.len()
    }

    pub(crate) fn track_index(&self, id: TrackId) -> Option<usize> {
        self.track_lookup.binary_search_by_key(&id, |e| e.0).ok().map(|i| self.track_lookup[i].1)
    }

    pub(crate) fn scene_index(&self, id: SceneId) -> Option<usize> {
        self.scene_lookup.binary_search_by_key(&id, |e| e.0).ok().map(|i| self.scene_lookup[i].1)
    }

    /// Apply parameter changes collected at compile time (audio thread,
    /// right after installation).
    pub(crate) fn apply_init_params(&mut self) {
        for t in &self.tracks {
            for_each_proc(&t.devices, &mut |p| {
                if !p.init_params.is_empty() {
                    for &(id, v) in &p.init_params {
                        set_param_guarded(&p.proc, id, v);
                    }
                }
            });
        }
    }

    /// Take over runtime state from the graph being replaced. Real-time safe.
    pub(crate) fn adopt(&mut self, old: &mut CompiledGraph) {
        for ti in 0..self.tracks.len() {
            let id = self.tracks[ti].id;
            let Some(oi) = old.track_index(id) else {
                // New while the old graph played: fade in from silence.
                let n = &mut self.tracks[ti];
                n.gain_primed = true;
                n.sends.iter_mut().for_each(|s| s.primed = true);
                continue;
            };
            let o = &mut old.tracks[oi];
            let n = &mut self.tracks[ti];
            std::mem::swap(&mut n.active, &mut o.active);
            n.rng = o.rng;
            n.release_all = o.release_all;
            n.cur_gain = o.cur_gain;
            n.gain_primed = o.gain_primed;
            n.input_sel = o.input_sel;
            n.no_input = o.no_input;
            n.meter_acc.rms = o.meter_acc.rms;
            n.out_delay.adopt(&mut o.out_delay);
            n.clip_delay.adopt(&mut o.clip_delay);
            for (k, ns) in n.sends.iter_mut().enumerate() {
                match o.sends.get_mut(k) {
                    Some(os) if ns.target_id_matches(os) => {
                        ns.cur = os.cur;
                        ns.primed = os.primed;
                        ns.delay.adopt(&mut os.delay);
                    }
                    // Added or redirected while playing: fade in from silence.
                    _ => ns.primed = true,
                }
            }
            adopt_devices(&mut n.devices, &mut o.devices);
            n.panic_notes |= midi_lost(&o.devices, &n.devices);
            let map_scene = |s: usize| old.scenes.get(s).and_then(|sid| {
                self.scene_lookup.binary_search_by_key(sid, |e| e.0).ok().map(|i| self.scene_lookup[i].1)
            });
            let os = old.tracks[oi].session;
            let n = &mut self.tracks[ti];
            n.session.override_arrangement = os.override_arrangement;
            n.session.playing = os.playing.and_then(|(s, start)| map_scene(s).map(|ns| (ns, start)));
            n.session.queued = os.queued.and_then(|q| match q.scene {
                None => Some(q),
                Some(s) => map_scene(s).map(|ns| Queued { scene: Some(ns), at: q.at }),
            });
            // A launched slot that lost its clip stops.
            if let Some((s, _)) = n.session.playing
                && n.slots.get(s).copied().flatten().is_none() {
                    n.session.playing = None;
                    n.release_all = true;
                }
        }
        self.out_acc.rms = old.out_acc.rms;
    }

    /// Flag every track to release its clip notes at the next segment.
    pub(crate) fn release_all_notes(&mut self) {
        for t in &mut self.tracks {
            t.release_all = true;
            // The playhead jumped: a groove change waiting for it is due now.
            for c in &mut t.clips {
                if let Some(g) = c.groove.as_mut() {
                    g.settle();
                }
            }
        }
    }

    pub(crate) fn stop_all_session(&mut self) {
        for t in &mut self.tracks {
            if t.session.playing.is_some() {
                t.release_all = true;
            }
            t.session.playing = None;
            t.session.queued = None;
        }
    }

    pub(crate) fn back_to_arrangement(&mut self) {
        self.stop_all_session();
        for t in &mut self.tracks {
            t.session.override_arrangement = false;
        }
    }

    fn quantize_up(&self, now: f64, q: Quantize) -> Tick {
        let now_t = now.ceil() as Tick;
        if q == Quantize::None {
            return now_t;
        }
        let sig_start = self.tempo.signatures.iter().rev().find(|(s, _)| *s as f64 <= now).map_or(0, |(s, _)| *s);
        let step = q.ticks(self.tempo.signature_at(now_t).bar_ticks()).max(1);
        let rel = now_t - sig_start;
        sig_start + (rel + step - 1).div_euclid(step) * step
    }

    /// Queue a clip launch (or a stop when the slot is empty).
    pub(crate) fn launch(&mut self, track: usize, scene: usize, now: f64, immediate: bool, global_q: Quantize) {
        let Some(slot) = self.tracks.get(track).and_then(|t| t.slots.get(scene)).copied() else { return };
        let q = slot.map_or(global_q, |c| self.tracks[track].clips[c].quantize);
        let at = if immediate { now.floor() as Tick } else { self.quantize_up(now, q) };
        let t = &mut self.tracks[track];
        match slot {
            Some(c) => {
                let toggle = t.clips[c].mode == LaunchMode::Toggle && t.session.playing.is_some_and(|(s, _)| s == scene);
                t.session.queued = Some(Queued { scene: if toggle { None } else { Some(scene) }, at });
            }
            None => {
                if t.session.playing.is_some() {
                    t.session.queued = Some(Queued { scene: None, at });
                }
            }
        }
        t.session.override_arrangement = true;
    }

    pub(crate) fn launch_scene(&mut self, scene: usize, now: f64, immediate: bool, global_q: Quantize) {
        for ti in 0..self.tracks.len() {
            if self.tracks[ti].kind.has_clips() {
                self.launch(ti, scene, now, immediate, global_q);
            }
        }
    }

    pub(crate) fn stop_track(&mut self, track: usize, now: f64, global_q: Quantize) {
        let at = self.quantize_up(now, global_q);
        if let Some(t) = self.tracks.get_mut(track) {
            if t.session.playing.is_some() {
                t.session.queued = Some(Queued { scene: None, at });
            }
            t.session.override_arrangement = true;
        }
    }

    /// Queue a live event for `track`, `offset` frames into the block being
    /// built (see [`SegmentIo::out_offset`] for how segments take theirs).
    pub(crate) fn push_live(&mut self, track: usize, offset: u32, kind: EventKind) {
        if let Some(t) = self.tracks.get_mut(track) {
            t.live.push(offset, kind);
        }
    }

    /// End of a block: live events no segment took (a track that carries no
    /// audio) are dropped.
    pub(crate) fn clear_live(&mut self) {
        for t in &mut self.tracks {
            t.live.clear();
        }
    }

    /// Fast path: a device parameter (normalised) on the installed graph.
    pub(crate) fn set_device_param(&mut self, device: DeviceId, param: u32, v: f32) {
        for t in &mut self.tracks {
            if let Some(p) = find_proc_mut(&mut t.devices, device) {
                set_param_guarded(&p.proc, param, v);
                return;
            }
        }
    }

    /// Fast path: the global groove amount for every clip that has a groove.
    pub(crate) fn set_groove_amount(&mut self, amount: f32, now: Tick) {
        for c in self.tracks.iter_mut().flat_map(|t| t.clips.iter_mut()) {
            if let Some(g) = c.groove.as_mut() {
                g.set_amount(amount, now);
            }
        }
    }

    /// Fast path: one pool groove's settings.
    pub(crate) fn set_groove_settings(&mut self, id: aitrack_model::GrooveId, settings: aitrack_model::GrooveSettings, now: Tick) {
        for c in self.tracks.iter_mut().flat_map(|t| t.clips.iter_mut()) {
            if let Some(g) = c.groove.as_mut().filter(|g| g.id == id) {
                g.set_settings(settings, now);
            }
        }
    }

    /// Fast path: a rack's Chain Select value.
    pub(crate) fn set_rack_selector(&mut self, rack: DeviceId, value: u8) {
        fn find(devs: &mut [DeviceNode], id: DeviceId) -> Option<&mut RackNode> {
            for d in devs {
                if let DeviceNode::Rack(r) = d {
                    if r.id == id {
                        return Some(r);
                    }
                    for c in r.chains.iter_mut().chain(r.returns.iter_mut()) {
                        if let Some(found) = find(&mut c.devices, id) {
                            return Some(found);
                        }
                    }
                }
            }
            None
        }
        for t in &mut self.tracks {
            if let Some(r) = find(&mut t.devices, rack) {
                r.selector = value;
                return;
            }
        }
    }

    pub(crate) fn set_track_volume(&mut self, track: TrackId, db: f32) {
        if let Some(i) = self.track_index(track) {
            self.tracks[i].volume_db = db;
        }
    }

    /// Pick the device channels of an audio track (`None` = default input).
    pub(crate) fn set_track_input(&mut self, track: TrackId, sel: Option<(u16, u16)>) {
        if let Some(i) = self.track_index(track) {
            self.tracks[i].input_sel = sel;
        }
    }

    pub(crate) fn set_track_no_input(&mut self, track: TrackId, none: bool) {
        if let Some(i) = self.track_index(track) {
            self.tracks[i].no_input = none;
        }
    }

    pub(crate) fn set_track_monitor(&mut self, track: TrackId, monitor: Monitor) {
        if let Some(i) = self.track_index(track) {
            self.tracks[i].monitor = monitor;
        }
    }

    pub(crate) fn set_track_pan(&mut self, track: TrackId, pan: f32) {
        if let Some(i) = self.track_index(track) {
            self.tracks[i].pan = pan;
        }
    }

    pub(crate) fn set_send_level(&mut self, track: TrackId, index: u16, db: f32) {
        if let Some(i) = self.track_index(track)
            && let Some(s) = self.tracks[i].sends.iter_mut().find(|s| s.index == index)
        {
            s.level_db = db;
        }
    }

    /// Publish meters and session state (audio thread, once per block).
    pub(crate) fn publish(&mut self, ticks_now: f64) {
        let sr = self.sample_rate;
        for t in &mut self.tracks {
            if let Some(m) = t.meter {
                let (peak, rms) = t.meter_acc.finish(sr);
                self.meters.slots[m].publish(peak, rms);
            }
            if let Some(si) = t.session_index {
                let st = &self.session_status.state[si];
                let playing = t.session.playing;
                st.playing.store(playing.map_or(NO_SCENE, |(s, _)| s as i32), Ordering::Relaxed);
                st.queued.store(
                    t.session.queued.map_or(NO_SCENE, |q| q.scene.map_or(STOP_SCENE, |s| s as i32)),
                    Ordering::Relaxed,
                );
                let progress = playing
                    .and_then(|(s, start)| t.slots.get(s).copied().flatten().map(|c| (c, start)))
                    .map_or(0.0, |(c, start)| {
                        let cyc = t.clips[c].session_cycle() as f64;
                        ((ticks_now - start as f64).max(0.0) % cyc / cyc) as f32
                    });
                st.progress.store(progress.to_bits(), Ordering::Relaxed);
            }
        }
        let (peak, rms) = self.out_acc.finish(sr);
        self.meters.master.publish(peak, rms);
        // Device display data, only for the devices a UI is looking at.
        let mut scratch = [0.0f32; DEVICE_METER_LEN];
        let mut midi_dropped = 0u64;
        for t in &self.tracks {
            self.new_crashes += drain_crashes(&t.devices);
            for_each_proc(&t.devices, &mut |p| {
                if let Some(m) = p.midi.as_deref() {
                    // SAFETY: audio thread owns the installed graph.
                    midi_dropped += m.dropped as u64 + guarded(|| unsafe { p.proc.get_mut() }.dropped_events()).unwrap_or(0);
                }
            });
            for_each_proc(&t.devices, &mut |p| {
                if p.meter.wanted() && !p.proc.is_crashed() {
                    // SAFETY: audio thread owns the installed graph.
                    let proc = unsafe { p.proc.get_mut() };
                    match guarded(|| proc.meter(&mut scratch)) {
                        Some(n) => p.meter.publish(&scratch[..n.min(DEVICE_METER_LEN)]),
                        // Counted by the next block's drain.
                        None => p.proc.mark_crashed(),
                    }
                }
            });
        }
        self.midi_dropped = midi_dropped;
    }

    /// Process one segment (no transport discontinuity inside it).
    pub(crate) fn process_segment(&mut self, io: &mut SegmentIo<'_>) {
        let n = io.frames;
        let sr = self.sample_rate;
        let Self { tracks, inputs, side, out_bus, tempo, scenes, out_acc, sanitized, .. } = self;
        let ctx = TimeCtx { tempo, sr, seg_start: io.seg_start };
        let t0 = ctx.tick_of(io.seg_start);
        let t1 = ctx.tick_of(io.seg_start + n as i64);
        let sig = tempo.signature_at(t0.floor() as Tick);
        let transport = Transport {
            playing: io.playing,
            recording: io.recording,
            bpm: tempo.bpm_at(t0.floor() as Tick),
            time_sig: (sig.numerator, sig.denominator),
            position_beats: t0 / PPQ as f64,
            position_samples: io.seg_start,
            loop_range_beats: io.loop_range.map(|(a, b)| (a as f64 / PPQ as f64, b as f64 / PPQ as f64)),
        };

        for (i, t) in tracks.iter().enumerate() {
            if t.has_input {
                inputs[i].set_frames(n);
                inputs[i].clear();
            }
        }
        out_bus.set_frames(n);
        out_bus.clear();

        // VCA pre-pass: VCAs evaluate their fader, members read it.
        for t in tracks.iter_mut().filter(|t| t.kind == TrackKind::Vca) {
            let at = |tick: f64| {
                t.volume_lane.as_ref().and_then(|l| lane_value(l, tick)).map_or(t.volume_db, volume_db_from_normalized)
            };
            t.vca_out = (at(t0), at(t1), t.is_muted_at(t0, false));
        }
        for i in 0..tracks.len() {
            if let Some(v) = tracks[i].vca {
                let val = tracks[v].vca_out;
                tracks[i].vca_in = val;
            }
        }

        for (i, t) in tracks.iter_mut().enumerate() {
            if !t.carries_audio() {
                continue;
            }
            process_track(t, i, &ctx, &*io, &transport, inputs, side, out_bus, scenes.len());
        }

        // The last line of defence: whatever a device, a clip or an automation
        // curve did upstream, the hardware never gets a NaN or an infinity.
        *sanitized += sanitize(out_bus);
        out_acc.add(out_bus);
        let out = &mut *io.out;
        for ch in 0..out.channels().min(out_bus.channels()) {
            let dst = &mut out.channel_mut(ch)[io.out_offset..io.out_offset + n];
            dst.copy_from_slice(out_bus.channel(ch));
        }
    }
}

impl SendNode {
    fn target_id_matches(&self, other: &SendNode) -> bool {
        self.target_id == other.target_id && self.pre_fader == other.pre_fader && self.delay.delay() == other.delay.delay()
    }
}

#[allow(clippy::too_many_arguments)]
fn process_track(
    t: &mut TrackNode,
    index: usize,
    ctx: &TimeCtx<'_>,
    io: &SegmentIo<'_>,
    transport: &Transport,
    inputs: &mut [AudioBuffer],
    side: &mut [AudioBuffer],
    out_bus: &mut AudioBuffer,
    scene_count: usize,
) {
    let n = io.frames;
    let sr = ctx.sr;
    let lo = io.seg_start;
    let hi = lo + n as i64;

    t.buf.set_frames(n);
    if t.has_input {
        t.buf.copy_from(&inputs[index]);
    } else {
        t.buf.clear();
    }

    // Events: pending releases, live input, clip playback.
    t.events.clear();
    if t.release_all {
        t.active.release_all(0, &mut t.events);
        t.release_all = false;
        // What MIDI effects left open or pending goes with it.
        flush_midi(&mut t.devices);
    }
    if std::mem::take(&mut t.panic_notes) {
        t.events.push(0, EventKind::AllNotesOff);
    }
    // Live input: the events whose block offset falls in this segment, at
    // their offset inside it.
    if !t.live.events.is_empty() {
        let (seg_lo, seg_hi) = (io.out_offset as u32, (io.out_offset + n) as u32);
        let events = &mut t.events;
        t.live.events.retain(|e| {
            let mine = e.offset >= seg_lo && e.offset < seg_hi;
            if mine {
                events.push(e.offset - seg_lo, e.kind);
            }
            !mine
        });
    }
    if t.kind.has_clips() {
        let use_clip_buf = t.clip_delay.is_active();
        if use_clip_buf {
            t.clip_buf.set_frames(n);
            t.clip_buf.clear();
        }
        if io.playing {
            play_clips(t, ctx, lo, hi, scene_count, use_clip_buf);
        }
        t.active.release_due(ctx, hi, &mut t.events);
        if use_clip_buf {
            t.clip_delay.process(&mut t.clip_buf);
            t.buf.add_from(&t.clip_buf, 1.0);
        }
        // Input monitoring for armed audio tracks. In: always; Auto: unless
        // the transport plays back without recording; Off: never.
        let monitoring = match t.monitor {
            Monitor::In => true,
            Monitor::Auto => !io.playing || io.recording,
            Monitor::Off => false,
        };
        if t.kind == TrackKind::Audio && t.arm && !t.no_input && monitoring {
            let own = match (t.input_sel, io.raw) {
                (Some((first, count)), Some((raw, channels))) if t.in_buf.capacity() >= n => {
                    t.in_buf.set_frames(n);
                    let (l, r) = t.in_buf.stereo_mut();
                    crate::record::extract_input(raw, channels, io.out_offset, n, first as usize, count as usize, l, r);
                    true
                }
                _ => false,
            };
            let src = if own { Some((&t.in_buf, 0)) } else { io.input };
            if let Some((input, off)) = src {
                for ch in 0..2.min(input.channels()) {
                    let src = &input.channel(ch.min(input.channels() - 1))[off..off + n];
                    for (d, s) in t.buf.channel_mut(ch).iter_mut().zip(src) {
                        *d += *s;
                    }
                }
            }
        }
    }
    t.events.sort();

    // Device chain. Automated device parameters become timed events in the
    // device's own node; only a device switch cuts the chain into blocks.
    let auto_start = lo - t.chain_latency_in as i64;
    for pa in t.param_autos.iter() {
        pa.for_each_device(|device| {
            if let Some(node) = find_proc_mut(&mut t.devices, device) {
                node.auto_events.clear();
            }
        });
    }
    for pa in t.param_autos.iter_mut() {
        pa.emit(&mut t.devices, ctx, auto_start, n);
    }
    for pa in t.param_autos.iter() {
        pa.for_each_device(|device| {
            if let Some(node) = find_proc_mut(&mut t.devices, device) {
                node.auto_events.sort();
            }
        });
    }
    let allow_sleep = !io.playing || t.param_autos.is_empty();
    if t.param_autos.iter().all(ParamAuto::is_timed) {
        let mut si = SideIn { bufs: &*side, scratch: &mut t.side_scratch, offset: 0 };
        process_devices(&mut t.devices, &mut t.buf, &t.events.events, transport, sr, &mut si, (0, n), allow_sleep);
    } else {
        let mut a = 0;
        while a < n {
            let b = (a + CONTROL_BLOCK).min(n);
            let tick = ctx.tick_of(auto_start + a as i64);
            for pa in &mut t.param_autos {
                pa.apply(tick);
            }
            t.chunk_buf.set_frames(b - a);
            for ch in 0..t.buf.channels().min(t.chunk_buf.channels()) {
                t.chunk_buf.channel_mut(ch).copy_from_slice(&t.buf.channel(ch)[a..b]);
            }
            t.chunk_events.clear();
            for e in t.events.events.iter().filter(|e| (e.offset as usize) >= a && (e.offset as usize) < b) {
                t.chunk_events.push(e.offset - a as u32, e.kind);
            }
            let mut tr = *transport;
            tr.position_samples = lo + a as i64;
            tr.position_beats = ctx.tick_of(lo + a as i64) / PPQ as f64;
            let mut si = SideIn { bufs: &*side, scratch: &mut t.side_scratch, offset: a };
            process_devices(&mut t.devices, &mut t.chunk_buf, &t.chunk_events.events, &tr, sr, &mut si, (a, b), allow_sleep);
            for ch in 0..t.buf.channels().min(t.chunk_buf.channels()) {
                t.buf.channel_mut(ch)[a..b].copy_from_slice(t.chunk_buf.channel(ch));
            }
            a = b;
        }
    }
    // Publish this track's signal (after its devices, before its fader) for
    // the devices that sidechain from it.
    if let Some(slot) = t.side_out
        && let Some(out) = side.get_mut(slot)
    {
        out.set_frames(n);
        out.copy_from(&t.buf);
    }

    // Pre-fader sends, fader, post-fader sends, all at control rate.
    let fader_shift = t.fader_latency as i64;
    let (vca0, vca1, vca_mute) = t.vca_in;
    let has_vca = t.vca.is_some();
    for pass in [true, false] {
        for s in t.sends.iter_mut().filter(|s| s.pre_fader == pass) {
            s.buf.set_frames(n);
            let mut a = 0;
            while a < n {
                let b = (a + CONTROL_BLOCK).min(n);
                let tick = ctx.tick_of(lo + b as i64 - fader_shift);
                let muted = t.mute
                    || t.silenced
                    || (has_vca && vca_mute)
                    || t.mute_lane.as_ref().and_then(|l| lane_value(l, tick)).is_some_and(|v| v >= 0.5);
                let db = s.lane.as_ref().and_then(|l| lane_value(l, tick)).map_or(s.level_db, volume_db_from_normalized);
                let target = if muted { 0.0 } else { db_to_gain(db) };
                if !s.primed {
                    s.cur = target;
                    s.primed = true;
                }
                let step = (target - s.cur) / (b - a) as f32;
                for ch in 0..2 {
                    let src = &t.buf.channel(ch)[a..b];
                    let dst = &mut s.buf.channel_mut(ch)[a..b];
                    let mut g = s.cur;
                    for (d, x) in dst.iter_mut().zip(src) {
                        g += step;
                        *d = x * g;
                    }
                }
                s.cur = target;
                a = b;
            }
            s.delay.process(&mut s.buf);
            inputs[s.target].add_from(&s.buf, 1.0);
        }
        if pass {
            // The fader itself.
            let mut a = 0;
            while a < n {
                let b = (a + CONTROL_BLOCK).min(n);
                let tick = ctx.tick_of(lo + b as i64 - fader_shift);
                let vca_db = if has_vca { vca0 + (vca1 - vca0) * (b as f32 / n as f32) } else { 0.0 };
                let (tl, tr) = t.fader_target(tick, vca_db, has_vca && vca_mute);
                if !t.gain_primed {
                    t.cur_gain = [tl, tr];
                    t.gain_primed = true;
                }
                let len = (b - a) as f32;
                let (sl, sr_) = ((tl - t.cur_gain[0]) / len, (tr - t.cur_gain[1]) / len);
                let (bl, br) = t.buf.stereo_mut();
                let (mut gl, mut gr) = (t.cur_gain[0], t.cur_gain[1]);
                for k in a..b {
                    gl += sl;
                    gr += sr_;
                    bl[k] *= gl;
                    br[k] *= gr;
                }
                t.cur_gain = [tl, tr];
                a = b;
            }
        }
    }

    t.meter_acc.add(&t.buf);
    if let Some(a) = &t.analysis
        && a.wants_track(t.id)
    {
        a.write(t.buf.channel(0), t.buf.channel(1));
    }
    if let Some(tap) = t.tap.as_deref_mut() {
        tap.scratch.set_frames(n);
        tap.scratch.copy_from(&t.buf);
        tap.delay.process(&mut tap.scratch);
        for ch in 0..2 {
            tap.block.channel_mut(ch)[io.out_offset..io.out_offset + n].copy_from_slice(tap.scratch.channel(ch));
        }
    }
    t.out_delay.process(&mut t.buf);
    match t.output {
        OutputTarget::Track(j) => inputs[j].add_from(&t.buf, 1.0),
        OutputTarget::Out => out_bus.add_from(&t.buf, 1.0),
        OutputTarget::None => {}
    }
}

/// Session and arrangement playback for one segment.
fn play_clips(t: &mut TrackNode, ctx: &TimeCtx<'_>, lo: i64, hi: i64, scene_count: usize, use_clip_buf: bool) {
    let mut cur = lo;
    // Session View: play the launched clip, applying queued launches/stops
    // (and follow actions / clip ends) at their exact sample.
    for _guard in 0..8 {
        if let Some((scene, start)) = t.session.playing
            && let Some(c) = t.slots.get(scene).copied().flatten() {
                let clip = &t.clips[c];
                let bar = ctx.tempo.signature_at(start).bar_ticks();
                let mut auto: Option<Queued> = None;
                if let Some(f) = clip.follow {
                    let (bars, target) = follow_target(f, scene, &t.slots, scene_count, &mut t.rng);
                    auto = Some(Queued { scene: target, at: start + bars as Tick * bar });
                }
                if clip.looping.is_none() {
                    let end = Queued { scene: None, at: start + clip.length };
                    if auto.is_none_or(|a| a.at > end.at) {
                        auto = Some(end);
                    }
                }
                if let Some(a) = auto
                    && t.session.queued.is_none_or(|q| q.at > a.at) {
                        t.session.queued = Some(a);
                    }
            }
        let next = t.session.queued.filter(|q| ctx.sample_of(q.at) < hi);
        let seg_hi = next.map_or(hi, |q| ctx.sample_of(q.at).clamp(cur, hi));
        if let Some((scene, start)) = t.session.playing
            && let Some(c) = t.slots.get(scene).copied().flatten() {
                let clip = &t.clips[c];
                let limit = if clip.looping.is_some() { None } else { Some(clip.length) };
                let buf = if use_clip_buf { &mut t.clip_buf } else { &mut t.buf };
                clip.schedule_midi(start, limit, cur, seg_hi, ctx, &mut t.events, &mut t.active, &mut t.rng);
                clip.render_audio(start, limit, cur, seg_hi, ctx, buf);
            }
        let Some(q) = next else { break };
        t.active.release_all((seg_hi - ctx.seg_start) as u32, &mut t.events);
        t.session.playing = q.scene.filter(|s| t.slots.get(*s).copied().flatten().is_some()).map(|s| (s, q.at));
        t.session.queued = None;
        cur = seg_hi;
    }

    if t.session.override_arrangement {
        return;
    }
    for p in &t.arrangement {
        let clip = &t.clips[p.clip];
        let s0 = ctx.sample_of(p.start);
        let s1 = ctx.sample_of(p.start + clip.length);
        let a = s0.max(lo);
        let b = s1.min(hi);
        if a >= b {
            continue;
        }
        let buf = if use_clip_buf { &mut t.clip_buf } else { &mut t.buf };
        clip.schedule_midi(p.start, Some(clip.length), a, b, ctx, &mut t.events, &mut t.active, &mut t.rng);
        clip.render_audio(p.start, Some(clip.length), a, b, ctx, buf);
    }
}

fn follow_target(f: FollowAction, scene: usize, slots: &[Option<usize>], scene_count: usize, rng: &mut Rng) -> (u32, Option<usize>) {
    let filled = |s: &usize| slots.get(*s).copied().flatten().is_some();
    let n = scene_count.min(slots.len()).max(1);
    match f {
        FollowAction::Next { after_bars } => (after_bars.max(1), (1..=n).map(|k| (scene + k) % n).find(filled)),
        FollowAction::Previous { after_bars } => (after_bars.max(1), (1..=n).map(|k| (scene + n - k) % n).find(filled)),
        FollowAction::First { after_bars } => (after_bars.max(1), (0..n).find(filled)),
        FollowAction::Random { after_bars } => {
            let start = (rng.next_f32() * n as f32) as usize;
            (after_bars.max(1), (0..n).map(|k| (start + k) % n).find(filled))
        }
        FollowAction::Stop { after_bars } => (after_bars.max(1), None),
    }
}

#[cfg(test)]
#[path = "tests/plugin_automation.rs"]
mod plugin_automation;

#[cfg(test)]
#[path = "tests/engine_safety.rs"]
mod engine_safety;

#[cfg(test)]
#[path = "tests/fast_path.rs"]
mod fast_path;

#[cfg(test)]
#[path = "tests/recording.rs"]
mod recording;

#[cfg(test)]
#[path = "tests/racks.rs"]
mod racks;

#[cfg(test)]
#[path = "tests/midi_fx.rs"]
mod midi_fx;

#[cfg(test)]
#[path = "tests/stock_midi.rs"]
mod stock_midi;

#[cfg(test)]
#[path = "tests/groove.rs"]
mod groove;
