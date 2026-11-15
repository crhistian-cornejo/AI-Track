//! The pieces of a MIDI effect: what it is given ([`MidiContext`]), where it
//! writes ([`EventSink`]) and a fixed-capacity queue for notes it wants to
//! play *after* the block it is processing ([`EventQueue`]).
//!
//! A processor whose [`PluginInfo::category`](crate::PluginInfo) is
//! [`Category::MidiEffect`](crate::Category) is not asked for audio: the
//! engine calls [`Processor::process_midi`](crate::Processor::process_midi)
//! with the events that reach it and hands whatever it writes to the next
//! device of the chain. Nothing here allocates after construction.

use crate::event::{Event, EventKind};
use crate::Transport;

/// What a MIDI effect sees for one block (or one piece of a block).
pub struct MidiContext<'a> {
    pub sample_rate: f64,
    /// Frames of the block: every offset in `events` is below it, and every
    /// offset written to the sink must be too.
    pub frames: usize,
    /// The events reaching this device, sorted by `offset`.
    pub events: &'a [Event],
    pub transport: &'a Transport,
}

/// Where a MIDI effect writes its output: a fixed-capacity list. Writing past
/// the capacity drops the event and counts it.
pub struct EventSink<'a> {
    events: &'a mut Vec<Event>,
    dropped: &'a mut u32,
}

impl<'a> EventSink<'a> {
    /// A sink over `events`, whose spare capacity is the limit (it never grows).
    pub fn new(events: &'a mut Vec<Event>, dropped: &'a mut u32) -> Self {
        Self { events, dropped }
    }

    /// Append an event; `false` (and counted) when the list is full.
    #[inline]
    pub fn push(&mut self, offset: u32, kind: EventKind) -> bool {
        if self.events.len() < self.events.capacity() {
            self.events.push(Event { offset, kind });
            true
        } else {
            *self.dropped = self.dropped.saturating_add(1);
            false
        }
    }

    /// Events written so far.
    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Events lost to a full list since the engine last reset the count.
    pub fn dropped(&self) -> u32 {
        *self.dropped
    }
}

/// Events a MIDI effect will emit later than the block they were created in
/// (an arpeggiator's next steps, an echo's repeats), kept in time order in a
/// preallocated list.
///
/// Time is the queue's own clock, counted in processed frames: it advances
/// by `frames` on every [`drain_due`](Self::drain_due), so it runs the same
/// way live and in an offline render, and keeps running while the transport
/// is stopped (an echo of a note played live still repeats).
#[derive(Clone, Debug)]
pub struct EventQueue {
    items: Vec<(i64, EventKind)>,
    clock: i64,
    dropped: u64,
}

impl EventQueue {
    /// Room for `capacity` pending events: the clear limit of the queue.
    /// Allocates, so call it from `prepare`.
    pub fn with_capacity(capacity: usize) -> Self {
        Self { items: Vec::with_capacity(capacity), clock: 0, dropped: 0 }
    }

    pub fn capacity(&self) -> usize {
        self.items.capacity()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Events refused because the queue was full, since construction.
    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Forget everything pending (the transport stopped or jumped, the
    /// device was reset). The clock and the drop count stay.
    pub fn clear(&mut self) {
        self.items.clear();
    }

    /// Queue `kind` to be emitted `delay` frames after sample `offset` of the
    /// block being processed (call it before this block's `drain_due`).
    /// `false` (counted) when the queue is full.
    pub fn schedule(&mut self, offset: u32, delay: i64, kind: EventKind) -> bool {
        if self.items.len() >= self.items.capacity() {
            self.dropped += 1;
            return false;
        }
        let due = self.clock + offset as i64 + delay.max(0);
        // After the events already due at or before `due`: first in, first out.
        let at = self.items.partition_point(|(d, _)| *d <= due);
        self.items.insert(at, (due, kind));
        true
    }

    /// Emit into `out` every event due inside this block of `frames` frames,
    /// at its offset, then move the clock to the next block.
    pub fn drain_due(&mut self, frames: usize, out: &mut EventSink<'_>) {
        let end = self.clock + frames as i64;
        let n = self.items.partition_point(|(d, _)| *d < end);
        for (due, kind) in self.items.drain(..n) {
            out.push((due - self.clock).max(0) as u32, kind);
        }
        self.clock = end;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on(key: u8) -> EventKind {
        EventKind::NoteOn { channel: 0, key, velocity: 1.0, note_id: -1 }
    }

    fn sink_run(q: &mut EventQueue, frames: usize, cap: usize) -> (Vec<(u32, u8)>, u32) {
        let mut v = Vec::with_capacity(cap);
        let mut dropped = 0;
        q.drain_due(frames, &mut EventSink::new(&mut v, &mut dropped));
        (
            v.iter()
                .map(|e| match e.kind {
                    EventKind::NoteOn { key, .. } => (e.offset, key),
                    _ => (e.offset, 0),
                })
                .collect(),
            dropped,
        )
    }

    #[test]
    fn events_come_out_in_the_block_they_are_due_in_at_their_offset() {
        let mut q = EventQueue::with_capacity(8);
        q.schedule(10, 300, on(60));
        q.schedule(0, 100, on(61));
        q.schedule(10, 300, on(62));
        assert_eq!(sink_run(&mut q, 256, 8).0, vec![(100, 61)]);
        // 310 − 256 = 54, in order of arrival at equal times.
        assert_eq!(sink_run(&mut q, 256, 8).0, vec![(54, 60), (54, 62)]);
        assert!(q.is_empty());
    }

    #[test]
    fn a_full_queue_drops_and_counts() {
        let mut q = EventQueue::with_capacity(2);
        assert!(q.schedule(0, 10, on(1)) && q.schedule(0, 10, on(2)));
        assert!(!q.schedule(0, 10, on(3)));
        assert_eq!(q.dropped(), 1);
        assert_eq!(q.capacity(), 2, "never grows");
        q.clear();
        assert!(q.is_empty());
    }

    #[test]
    fn a_full_sink_drops_and_counts() {
        let mut v = Vec::with_capacity(1);
        let mut dropped = 0;
        let mut s = EventSink::new(&mut v, &mut dropped);
        assert!(s.push(0, on(1)));
        assert!(!s.push(1, on(2)));
        assert_eq!(s.dropped(), 1);
    }
}
