//! Test rig for the MIDI devices: feeds a [`Processor`] timed events in
//! blocks of any size and logs what comes out at absolute sample times.

use aitrack_dsp::{Event, EventKind, EventSink, MidiContext, Processor, Transport};

pub const SR: f64 = 48_000.0;

pub fn on(key: u8, velocity: f32) -> EventKind {
    EventKind::NoteOn { channel: 0, key, velocity, note_id: key as i32 }
}

pub fn off(key: u8) -> EventKind {
    EventKind::NoteOff { channel: 0, key, velocity: 0.0, note_id: key as i32 }
}

/// What the rig saw leave the device: `(sample, is_on, key, velocity)`.
pub type Note = (i64, bool, u8, f32);

pub struct Rig {
    pub p: Box<dyn Processor>,
    pub bpm: f64,
    pub playing: bool,
    /// Transport position at the next block (only advances while playing).
    pub pos_beats: f64,
    pub pos_samples: i64,
    /// Samples fed so far: the time axis of the log.
    pub clock: i64,
    pub log: Vec<(i64, EventKind)>,
    out: Vec<Event>,
    pub dropped: u32,
}

impl Rig {
    pub fn new(p: Box<dyn Processor>) -> Self {
        let mut r = Self { p, bpm: 120.0, playing: true, pos_beats: 0.0, pos_samples: 0, clock: 0, log: Vec::new(), out: Vec::with_capacity(16_384), dropped: 0 };
        r.p.prepare(SR, 4096);
        r
    }

    /// Start the transport at `beats` (the samples follow at the current tempo).
    pub fn start_at(&mut self, beats: f64) {
        self.pos_beats = beats;
        self.pos_samples = (beats * 60.0 / self.bpm * SR).round() as i64;
    }

    /// Feed `input` (`(sample since this call, event)`, sorted) for `frames`
    /// samples in blocks of `block`.
    pub fn run(&mut self, input: &[(i64, EventKind)], frames: i64, block: usize) {
        let mut done = 0i64;
        while done < frames {
            let n = (block as i64).min(frames - done) as usize;
            let evs: Vec<Event> = input.iter().filter(|(t, _)| *t >= done && *t < done + n as i64).map(|(t, k)| Event { offset: (t - done) as u32, kind: *k }).collect();
            let tr = Transport { playing: self.playing, bpm: self.bpm, position_beats: self.pos_beats, position_samples: self.pos_samples, ..Transport::default() };
            self.out.clear();
            let mut dropped = 0;
            let ctx = MidiContext { sample_rate: SR, frames: n, events: &evs, transport: &tr };
            self.p.process_midi(&ctx, &mut EventSink::new(&mut self.out, &mut dropped));
            self.dropped += dropped;
            let mut last = 0;
            for e in &self.out {
                assert!((e.offset as usize) < n, "offset {} outside a block of {n}", e.offset);
                assert!(e.offset >= last, "events out of order in a block");
                last = e.offset;
                self.log.push((self.clock + e.offset as i64, e.kind));
            }
            self.clock += n as i64;
            if self.playing {
                self.pos_samples += n as i64;
                self.pos_beats += n as f64 / SR * self.bpm / 60.0;
            }
            done += n as i64;
        }
    }

    /// The log with note ids zeroed: ids are per device instance, so two
    /// runs of equal music differ only there.
    pub fn log_no_ids(&self) -> Vec<(i64, EventKind)> {
        self.log
            .iter()
            .map(|(t, k)| {
                let k = match *k {
                    EventKind::NoteOn { channel, key, velocity, .. } => EventKind::NoteOn { channel, key, velocity, note_id: 0 },
                    EventKind::NoteOff { channel, key, velocity, .. } => EventKind::NoteOff { channel, key, velocity, note_id: 0 },
                    k => k,
                };
                (*t, k)
            })
            .collect()
    }

    /// Note-ons and note-offs in the log, times in samples.
    pub fn notes(&self) -> Vec<Note> {
        self.log
            .iter()
            .filter_map(|(t, k)| match *k {
                EventKind::NoteOn { key, velocity, .. } => Some((*t, true, key, velocity)),
                EventKind::NoteOff { key, velocity, .. } => Some((*t, false, key, velocity)),
                _ => None,
            })
            .collect()
    }

    /// `(sample, key)` of every note-on.
    pub fn ons(&self) -> Vec<(i64, u8)> {
        self.notes().into_iter().filter(|n| n.1).map(|n| (n.0, n.2)).collect()
    }

    pub fn keys(&self) -> Vec<u8> {
        self.ons().into_iter().map(|n| n.1).collect()
    }

    /// Every note-on has a note-off after it and none is left over.
    pub fn assert_balanced(&self) {
        let mut open: std::collections::BTreeMap<(u8, u8), i32> = Default::default();
        let mut last_on: std::collections::BTreeMap<(u8, u8), i64> = Default::default();
        for (t, k) in &self.log {
            match *k {
                EventKind::NoteOn { channel, key, .. } => {
                    *open.entry((channel, key)).or_default() += 1;
                    last_on.insert((channel, key), *t);
                }
                EventKind::NoteOff { channel, key, .. } => {
                    let c = open.entry((channel, key)).or_default();
                    *c -= 1;
                    assert!(*c >= 0, "note-off of {key} at {t} without a note-on");
                }
                EventKind::AllNotesOff => open.clear(),
                _ => {}
            }
        }
        open.retain(|_, c| *c != 0);
        assert!(open.is_empty(), "notes left open: {open:?} (last note-on at {last_on:?}, log ends at {})", self.clock);
    }
}
