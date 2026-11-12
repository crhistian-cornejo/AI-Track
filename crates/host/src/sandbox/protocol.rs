//! Wire format between the DAW and `aitrack-plugin-host`.
//!
//! Two channels:
//!
//! - **Control** (non-real-time): one JSON message per line on the child's
//!   stdin / stdout. Child → parent lines carry [`CONTROL_PREFIX`] so stray
//!   plugin output on stdout is ignored.
//! - **Audio** (real-time): one shared-memory [`Region`] per sandboxed
//!   instance plus two signals (`request`, `done`). See [`ShmHeader`].
//!
//! Same-cycle handoff, per block:
//!
//! 1. DAW audio thread writes input audio, events and transport, stores
//!    `seq = n` (release) and posts `request`.
//! 2. Sandbox RT thread wakes, processes the block with the plugin, writes
//!    output audio, parameter edits and latency, stores `done_seq = n`
//!    (release) and posts `done`.
//! 3. DAW waits for `done` until `done_seq == n` or the block budget
//!    (default 70 % of the block period) runs out. On time, outputs are
//!    copied in the *same* callback: no extra buffer of latency. Late: the
//!    node outputs silence for this block, and the next request is only
//!    issued once the sandbox caught up (`done_seq == seq`), so the two
//!    sides never touch the same buffers at once.

use std::sync::atomic::{AtomicU32, AtomicU64};

use aitrack_dsp::event::Expression;
use aitrack_dsp::{Event, EventKind, ParamInfo, ParamScale, Transport};
use serde::{Deserialize, Serialize};

use crate::info::HostedPluginInfo;
use crate::params::{ParamEdit, ParamEditKind};

pub const CONTROL_PREFIX: &str = "@@AITRACK_SBX@@";
pub const SHM_MAGIC: u32 = 0x4149_5342; // "AISB"
pub const SHM_VERSION: u32 = 1;
pub const MAX_EVENTS: usize = 1024;
pub const MAX_EDITS: usize = 256;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    Load { info: HostedPluginInfo },
    Prepare { sample_rate: f64, max_frames: u32, in_channels: u32, out_channels: u32, shm: String, request: String, done: String },
    SaveState,
    LoadState { data: Vec<u8> },
    OpenEditor,
    CloseEditor,
    Shutdown,
    /// Test hook: the host answers `Ok`, then its main thread stops (like an
    /// editor stuck in its own draw loop), so the parent's watchdog can be
    /// exercised.
    #[doc(hidden)]
    Freeze,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Response {
    Loaded { info: HostedPluginInfo, params: Vec<WireParam>, values: Vec<(u32, f32)> },
    Prepared { latency: u32 },
    State { data: Vec<u8> },
    Ok,
    Error { message: String },
    /// Heartbeat from the host's main loop, every [`HEARTBEAT`]. Not an
    /// answer to a request.
    Alive,
}

/// How often the host's main loop reports [`Response::Alive`].
pub const HEARTBEAT: std::time::Duration = std::time::Duration::from_millis(250);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WireParam {
    pub id: u32,
    pub name: String,
    pub short: String,
    pub unit: String,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub stepped: bool,
    pub automatable: bool,
}

impl WireParam {
    pub fn from_info(p: &ParamInfo) -> Self {
        Self {
            id: p.id,
            name: p.name.into(),
            short: p.short.into(),
            unit: p.unit.into(),
            min: p.min,
            max: p.max,
            default: p.default,
            stepped: p.scale == ParamScale::Stepped,
            automatable: p.automatable,
        }
    }

    pub fn to_info(&self) -> ParamInfo {
        use crate::strings::intern;
        let mut p = ParamInfo::new(self.id, intern(&self.name), self.min, self.max, self.default);
        p.short = intern(&self.short);
        p.unit = intern(&self.unit);
        if self.stepped {
            p.scale = ParamScale::Stepped;
        }
        p.automatable = self.automatable;
        p
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct ShmTransport {
    pub playing: u32,
    pub recording: u32,
    pub num: u32,
    pub den: u32,
    pub bpm: f64,
    pub position_beats: f64,
    pub position_samples: i64,
    pub loop_on: u32,
    pub _pad: u32,
    pub loop_start: f64,
    pub loop_end: f64,
}

impl ShmTransport {
    pub fn from(t: &Transport) -> Self {
        let (ls, le) = t.loop_range_beats.unwrap_or((0.0, 0.0));
        Self {
            playing: t.playing as u32,
            recording: t.recording as u32,
            num: t.time_sig.0 as u32,
            den: t.time_sig.1 as u32,
            bpm: t.bpm,
            position_beats: t.position_beats,
            position_samples: t.position_samples,
            loop_on: t.loop_range_beats.is_some() as u32,
            _pad: 0,
            loop_start: ls,
            loop_end: le,
        }
    }

    pub fn to(self) -> Transport {
        Transport {
            playing: self.playing != 0,
            recording: self.recording != 0,
            bpm: self.bpm,
            time_sig: (self.num as u8, self.den as u8),
            position_beats: self.position_beats,
            position_samples: self.position_samples,
            loop_range_beats: (self.loop_on != 0).then_some((self.loop_start, self.loop_end)),
        }
    }
}

/// Fixed header at offset 0 of the region.
#[repr(C)]
pub struct ShmHeader {
    pub magic: u32,
    pub version: u32,
    pub max_frames: u32,
    pub in_channels: u32,
    pub out_channels: u32,
    pub max_events: u32,
    pub max_edits: u32,
    pub _pad0: u32,
    // ── DAW → sandbox, valid once `seq` is published ──
    pub seq: AtomicU64,
    pub frames: u32,
    pub n_events: u32,
    pub sample_rate: f64,
    pub transport: ShmTransport,
    // ── sandbox → DAW, valid once `done_seq` is published ──
    pub done_seq: AtomicU64,
    pub n_edits: u32,
    pub latency: AtomicU32,
    pub tail: AtomicU32,
    /// 0 ok, 1 plugin crashed/disabled in the sandbox.
    pub status: AtomicU32,
    pub _pad1: [u32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct ShmEvent {
    pub offset: u32,
    pub kind: u32,
    pub a: u32,
    pub b: u32,
    pub c: i32,
    pub value: f32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct ShmEdit {
    pub id: u32,
    pub kind: u32,
    pub value: f32,
}

const fn align64(n: usize) -> usize {
    (n + 63) & !63
}

/// Byte offsets of each array inside a region.
#[derive(Clone, Copy, Debug)]
pub struct Layout {
    pub max_frames: usize,
    pub in_channels: usize,
    pub out_channels: usize,
    pub audio_in: usize,
    pub audio_out: usize,
    pub events: usize,
    pub edits: usize,
    pub size: usize,
}

impl Layout {
    pub fn new(max_frames: usize, in_channels: usize, out_channels: usize) -> Self {
        let audio_in = align64(std::mem::size_of::<ShmHeader>());
        let audio_out = align64(audio_in + in_channels * max_frames * 4);
        let events = align64(audio_out + out_channels * max_frames * 4);
        let edits = align64(events + MAX_EVENTS * std::mem::size_of::<ShmEvent>());
        let size = align64(edits + MAX_EDITS * std::mem::size_of::<ShmEdit>());
        Self { max_frames, in_channels, out_channels, audio_in, audio_out, events, edits, size }
    }
}

/// A mapped region with typed accessors. Each side only writes the parts
/// the protocol assigns to it.
pub struct Region {
    pub shm: super::shm::SharedMemory,
    pub layout: Layout,
}

impl Region {
    pub fn header(&self) -> &ShmHeader {
        unsafe { &*(self.shm.as_ptr() as *const ShmHeader) }
    }

    /// # Safety
    ///
    /// The caller must be the side the protocol lets write this part right
    /// now (DAW before publishing `seq`, sandbox before publishing
    /// `done_seq`) and must not keep two overlapping borrows alive.
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn header_mut(&self) -> &mut ShmHeader {
        unsafe { &mut *(self.shm.as_ptr() as *mut ShmHeader) }
    }

    /// # Safety
    ///
    /// The caller must be the side the protocol lets write this part right
    /// now (DAW before publishing `seq`, sandbox before publishing
    /// `done_seq`) and must not keep two overlapping borrows alive.
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn audio_in(&self, ch: usize) -> &mut [f32] {
        assert!(ch < self.layout.in_channels);
        let p = unsafe { self.shm.as_ptr().add(self.layout.audio_in + ch * self.layout.max_frames * 4) as *mut f32 };
        unsafe { std::slice::from_raw_parts_mut(p, self.layout.max_frames) }
    }

    /// # Safety
    ///
    /// The caller must be the side the protocol lets write this part right
    /// now (DAW before publishing `seq`, sandbox before publishing
    /// `done_seq`) and must not keep two overlapping borrows alive.
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn audio_out(&self, ch: usize) -> &mut [f32] {
        assert!(ch < self.layout.out_channels);
        let p = unsafe { self.shm.as_ptr().add(self.layout.audio_out + ch * self.layout.max_frames * 4) as *mut f32 };
        unsafe { std::slice::from_raw_parts_mut(p, self.layout.max_frames) }
    }

    /// # Safety
    ///
    /// The caller must be the side the protocol lets write this part right
    /// now (DAW before publishing `seq`, sandbox before publishing
    /// `done_seq`) and must not keep two overlapping borrows alive.
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn events(&self) -> &mut [ShmEvent] {
        let p = unsafe { self.shm.as_ptr().add(self.layout.events) as *mut ShmEvent };
        unsafe { std::slice::from_raw_parts_mut(p, MAX_EVENTS) }
    }

    /// # Safety
    ///
    /// The caller must be the side the protocol lets write this part right
    /// now (DAW before publishing `seq`, sandbox before publishing
    /// `done_seq`) and must not keep two overlapping borrows alive.
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn edits(&self) -> &mut [ShmEdit] {
        let p = unsafe { self.shm.as_ptr().add(self.layout.edits) as *mut ShmEdit };
        unsafe { std::slice::from_raw_parts_mut(p, MAX_EDITS) }
    }

    /// Initialise a freshly created region (DAW side).
    pub fn init(&self) {
        let l = self.layout;
        unsafe {
            std::ptr::write_bytes(self.shm.as_ptr(), 0, l.size);
            let h = self.header_mut();
            h.magic = SHM_MAGIC;
            h.version = SHM_VERSION;
            h.max_frames = l.max_frames as u32;
            h.in_channels = l.in_channels as u32;
            h.out_channels = l.out_channels as u32;
            h.max_events = MAX_EVENTS as u32;
            h.max_edits = MAX_EDITS as u32;
        }
    }

    pub fn validate(&self) -> bool {
        let h = self.header();
        h.magic == SHM_MAGIC
            && h.version == SHM_VERSION
            && h.max_frames as usize == self.layout.max_frames
            && h.in_channels as usize == self.layout.in_channels
            && h.out_channels as usize == self.layout.out_channels
    }
}

pub fn encode_event(e: &Event) -> ShmEvent {
    let mut s = ShmEvent { offset: e.offset, ..Default::default() };
    match e.kind {
        EventKind::NoteOn { channel, key, velocity, note_id } => {
            (s.kind, s.a, s.b, s.c, s.value) = (1, channel as u32, key as u32, note_id, velocity)
        }
        EventKind::NoteOff { channel, key, velocity, note_id } => {
            (s.kind, s.a, s.b, s.c, s.value) = (2, channel as u32, key as u32, note_id, velocity)
        }
        EventKind::NoteExpression { note_id, key, expr, value } => {
            let x = match expr {
                Expression::Tuning => 0,
                Expression::Pressure => 1,
                Expression::Pan => 2,
                Expression::Brightness => 3,
                Expression::Volume => 4,
            };
            (s.kind, s.a, s.b, s.c, s.value) = (3, x, key as u32, note_id, value)
        }
        EventKind::Param { id, value } => (s.kind, s.a, s.value) = (4, id, value),
        EventKind::ControlChange { channel, cc, value } => (s.kind, s.a, s.b, s.value) = (5, channel as u32, cc as u32, value),
        EventKind::PitchBend { channel, value } => (s.kind, s.a, s.value) = (6, channel as u32, value),
        EventKind::ChannelPressure { channel, value } => (s.kind, s.a, s.value) = (7, channel as u32, value),
        EventKind::AllNotesOff => s.kind = 8,
    }
    s
}

pub fn decode_event(s: &ShmEvent) -> Option<Event> {
    let kind = match s.kind {
        1 => EventKind::NoteOn { channel: s.a as u8, key: s.b as u8, velocity: s.value, note_id: s.c },
        2 => EventKind::NoteOff { channel: s.a as u8, key: s.b as u8, velocity: s.value, note_id: s.c },
        3 => {
            let expr = match s.a {
                0 => Expression::Tuning,
                1 => Expression::Pressure,
                2 => Expression::Pan,
                3 => Expression::Brightness,
                _ => Expression::Volume,
            };
            EventKind::NoteExpression { note_id: s.c, key: s.b as u8, expr, value: s.value }
        }
        4 => EventKind::Param { id: s.a, value: s.value },
        5 => EventKind::ControlChange { channel: s.a as u8, cc: s.b as u8, value: s.value },
        6 => EventKind::PitchBend { channel: s.a as u8, value: s.value },
        7 => EventKind::ChannelPressure { channel: s.a as u8, value: s.value },
        8 => EventKind::AllNotesOff,
        _ => return None,
    };
    Some(Event { offset: s.offset, kind })
}

pub fn encode_edit(e: &ParamEdit) -> ShmEdit {
    let kind = match e.kind {
        ParamEditKind::Begin => 0,
        ParamEditKind::Value => 1,
        ParamEditKind::End => 2,
    };
    ShmEdit { id: e.id, kind, value: e.value }
}

pub fn decode_edit(s: &ShmEdit) -> ParamEdit {
    let kind = match s.kind {
        0 => ParamEditKind::Begin,
        2 => ParamEditKind::End,
        _ => ParamEditKind::Value,
    };
    ParamEdit { id: s.id, value: s.value, kind }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_round_trip() {
        let evs = [
            Event { offset: 3, kind: EventKind::NoteOn { channel: 2, key: 60, velocity: 0.8, note_id: 7 } },
            Event { offset: 4, kind: EventKind::NoteExpression { note_id: 7, key: 60, expr: Expression::Pan, value: -0.5 } },
            Event { offset: 5, kind: EventKind::Param { id: 0xFFFF_0001, value: 0.25 } },
            Event { offset: 6, kind: EventKind::AllNotesOff },
        ];
        for e in evs {
            assert_eq!(decode_event(&encode_event(&e)), Some(e));
        }
    }

    #[test]
    fn layout_is_aligned() {
        let l = Layout::new(512, 2, 2);
        assert_eq!(l.audio_in % 64, 0);
        assert!(l.audio_out >= l.audio_in + 2 * 512 * 4);
        assert!(l.size >= l.edits + MAX_EDITS * std::mem::size_of::<ShmEdit>());
    }

    #[test]
    fn messages_serialise() {
        let r = Request::Prepare {
            sample_rate: 48e3,
            max_frames: 256,
            in_channels: 2,
            out_channels: 2,
            shm: "a".into(),
            request: "b".into(),
            done: "c".into(),
        };
        let s = serde_json::to_string(&r).unwrap();
        assert!(s.contains("\"type\":\"prepare\""));
        let _: Request = serde_json::from_str(&s).unwrap();
    }
}
