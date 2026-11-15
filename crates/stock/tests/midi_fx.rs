//! The stock MIDI effects through the registry: `process_midi` never touches
//! the heap, writes sorted in-range events, keeps keys and velocities valid
//! and leaves no note open, under default, extreme and scrambled parameters.
//!
//! `MIDI_IDS` lists the devices checked; extend it for new `midi.*` entries.

use aitrack_dsp::{Event, EventKind, EventSink, MidiContext, Processor, Transport};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};

const SR: f64 = 48_000.0;
const MIDI_IDS: &[&str] = &["midi.arp", "midi.chord", "midi.scale", "midi.velocity"];

struct Counting;

thread_local! {
    static ARMED: Cell<bool> = const { Cell::new(false) };
}
static COUNT: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ARMED.with(|a| a.get()) {
            COUNT.fetch_add(1, Ordering::SeqCst);
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if ARMED.with(|a| a.get()) {
            COUNT.fetch_add(1, Ordering::SeqCst);
        }
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if ARMED.with(|a| a.get()) {
            COUNT.fetch_add(1, Ordering::SeqCst);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

fn counted(f: impl FnOnce()) -> usize {
    let before = COUNT.load(Ordering::SeqCst);
    ARMED.with(|a| a.set(true));
    f();
    ARMED.with(|a| a.set(false));
    COUNT.load(Ordering::SeqCst) - before
}

fn xorshift(s: &mut u32) -> u32 {
    *s ^= *s << 13;
    *s ^= *s >> 17;
    *s ^= *s << 5;
    *s
}

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    Play,
    ReleaseAll,
    Quiet,
}

/// Everything preallocated: the loop it runs allocates nothing itself.
struct Driver {
    events: Vec<Event>,
    out: Vec<Event>,
    dropped: u32,
    transport: Transport,
    rng: u32,
    down: [bool; 16],
    block: usize,
    /// `(channel, key)` → open count, over everything written.
    open: BTreeMap<(u8, u8), i32>,
    blocks: u64,
}

impl Driver {
    fn new(block: usize) -> Self {
        Self {
            events: Vec::with_capacity(64),
            out: Vec::with_capacity(8192),
            dropped: 0,
            transport: Transport { playing: true, bpm: 128.0, ..Transport::default() },
            rng: 0xDEAD_BEEF,
            down: [false; 16],
            block,
            open: BTreeMap::new(),
            blocks: 0,
        }
    }

    /// One block: a few key presses and releases, then check the output.
    fn block(&mut self, p: &mut dyn Processor, mode: Mode) {
        self.events.clear();
        if mode == Mode::Quiet {
        } else if mode == Mode::ReleaseAll {
            for k in 0..16 {
                if self.down[k] {
                    self.down[k] = false;
                    self.events.push(Event { offset: 0, kind: EventKind::NoteOff { channel: 0, key: 48 + k as u8 * 2, velocity: 0.0, note_id: k as i32 } });
                }
            }
        } else if xorshift(&mut self.rng).is_multiple_of(3) {
            let k = (xorshift(&mut self.rng) % 16) as usize;
            let key = 48 + k as u8 * 2;
            let offset = xorshift(&mut self.rng) % self.block as u32;
            let kind = if self.down[k] {
                EventKind::NoteOff { channel: 0, key, velocity: 0.0, note_id: k as i32 }
            } else {
                EventKind::NoteOn { channel: 0, key, velocity: 0.1 + (xorshift(&mut self.rng) % 90) as f32 / 100.0, note_id: k as i32 }
            };
            self.down[k] = !self.down[k];
            self.events.push(Event { offset, kind });
        }
        self.out.clear();
        let ctx = MidiContext { sample_rate: SR, frames: self.block, events: &self.events, transport: &self.transport };
        p.process_midi(&ctx, &mut EventSink::new(&mut self.out, &mut self.dropped));
        self.blocks += 1;
        if self.transport.playing {
            self.transport.position_samples += self.block as i64;
            self.transport.position_beats += self.block as f64 / SR * self.transport.bpm / 60.0;
        }
    }

    fn check(&mut self, id: &str, what: &str) -> Result<(), String> {
        let mut last = 0;
        for e in &self.out {
            if e.offset as usize >= self.block {
                return Err(format!("{id} {what}: offset {} outside the block", e.offset));
            }
            if e.offset < last {
                return Err(format!("{id} {what}: events out of order"));
            }
            last = e.offset;
            match e.kind {
                EventKind::NoteOn { channel, key, velocity, .. } => {
                    if key > 127 || !(velocity > 0.0 && velocity <= 1.0) {
                        return Err(format!("{id} {what}: bad note-on key {key} velocity {velocity}"));
                    }
                    *self.open.entry((channel, key)).or_default() += 1;
                }
                EventKind::NoteOff { channel, key, .. } => {
                    let c = self.open.entry((channel, key)).or_default();
                    *c -= 1;
                    if *c < 0 {
                        return Err(format!("{id} {what}: note-off of {key} without a note-on"));
                    }
                }
                EventKind::AllNotesOff => self.open.clear(),
                _ => {}
            }
        }
        Ok(())
    }
}

fn scramble(p: &mut dyn Processor, pass: usize) {
    let ids: Vec<(u32, f32)> = p.info().params.iter().map(|i| (i.id, i.default_normalized())).collect();
    for (j, (id, default)) in ids.into_iter().enumerate() {
        let v = match pass {
            0 => default,
            1 => 0.0,
            2 => 1.0,
            3 => ((j * 7919 + 13) % 101) as f32 / 100.0,
            _ => ((j * 104_729 + 7) % 97) as f32 / 96.0,
        };
        p.set_param(id, v);
    }
}

#[test]
fn the_midi_devices_are_registered_as_midi_effects() {
    for id in MIDI_IDS {
        let entry = aitrack_stock::registry().iter().find(|e| e.id == *id).unwrap_or_else(|| panic!("{id} is not in the registry"));
        assert_eq!(entry.category, aitrack_dsp::Category::MidiEffect);
        assert_eq!((entry.create)().info().category, aitrack_dsp::Category::MidiEffect);
    }
}

#[test]
fn process_midi_never_allocates() {
    let mut failures = Vec::new();
    for id in MIDI_IDS {
        for block in [1usize, 128, 4096] {
            let mut p = aitrack_stock::create(id).unwrap();
            p.prepare(SR, 4096);
            let mut d = Driver::new(block);
            for pass in 0..5 {
                scramble(p.as_mut(), pass);
                for playing in [true, false] {
                    d.transport.playing = playing;
                    let rounds = (24_000 / block).clamp(8, 400);
                    let n = counted(|| {
                        for _ in 0..rounds {
                            d.block(p.as_mut(), Mode::Play);
                        }
                    });
                    if n > 0 {
                        failures.push(format!("{id} block {block} pass {pass} playing {playing}: {n} heap operations"));
                    }
                }
            }
            let mut meter = vec![0.0f32; 4096];
            let n = counted(|| {
                p.meter(&mut meter);
            });
            if n > 0 {
                failures.push(format!("{id} meter(): {n} heap operations"));
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn output_is_valid_and_every_note_is_closed() {
    let mut failures = Vec::new();
    for id in MIDI_IDS {
        for pass in 0..5 {
            for block in [32usize, 480] {
                let mut p = aitrack_stock::create(id).unwrap();
                p.prepare(SR, 4096);
                scramble(p.as_mut(), pass);
                let mut d = Driver::new(block);
                for _ in 0..2000 {
                    d.block(p.as_mut(), Mode::Play);
                    if let Err(e) = d.check(id, &format!("pass {pass} block {block}")) {
                        failures.push(e);
                        break;
                    }
                }
                // Let go of every key, drop Hold and give the gates time to end.
                if let Some(hold) = p.info().params.iter().find(|i| i.name == "Hold").map(|i| i.id) {
                    p.set_param(hold, 0.0);
                }
                d.block(p.as_mut(), Mode::ReleaseAll);
                let _ = d.check(id, "release");
                for _ in 0..(8 * 48_000 / block) {
                    d.block(p.as_mut(), Mode::Quiet);
                    if let Err(e) = d.check(id, &format!("tail pass {pass} block {block}")) {
                        failures.push(e);
                        break;
                    }
                }
                d.open.retain(|_, c| *c != 0);
                if !d.open.is_empty() {
                    failures.push(format!("{id} pass {pass} block {block}: notes left open {:?}", d.open));
                }
                if p.dropped_events() != 0 || d.dropped != 0 {
                    failures.push(format!("{id} pass {pass} block {block}: dropped events"));
                }
                // The state round trip must not panic, and reset empties.
                let state = p.save_state();
                p.load_state(&state);
                p.load_state(b"not json");
                p.reset();
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// Every factory preset of these devices does something audible with a held
/// chord (it starts notes) and leaves nothing open once the keys are let go.
#[test]
fn factory_presets_make_notes_and_close_them() {
    let mut failures = Vec::new();
    for id in MIDI_IDS {
        let presets = aitrack_stock::presets::list(id);
        assert!(presets.len() >= 6, "{id} has {} presets", presets.len());
        for r in presets {
            let preset = aitrack_stock::presets::load(&r.path).unwrap();
            let mut p = aitrack_stock::create(id).unwrap();
            p.prepare(SR, 4096);
            preset.apply(p.as_mut());
            let block = 256usize;
            let mut d = Driver::new(block);
            let mut ons = 0;
            for b in 0..(3 * 48_000 / block) {
                d.events.clear();
                if b == 0 {
                    for key in [60u8, 63, 67] {
                        d.events.push(Event { offset: 0, kind: EventKind::NoteOn { channel: 0, key, velocity: 0.8, note_id: key as i32 } });
                    }
                }
                if b == 2 * 48_000 / block {
                    for key in [60u8, 63, 67] {
                        d.events.push(Event { offset: 0, kind: EventKind::NoteOff { channel: 0, key, velocity: 0.0, note_id: key as i32 } });
                    }
                }
                d.out.clear();
                let ctx = MidiContext { sample_rate: SR, frames: block, events: &d.events, transport: &d.transport };
                p.process_midi(&ctx, &mut EventSink::new(&mut d.out, &mut d.dropped));
                d.transport.position_samples += block as i64;
                d.transport.position_beats += block as f64 / SR * d.transport.bpm / 60.0;
                ons += d.out.iter().filter(|e| matches!(e.kind, EventKind::NoteOn { .. })).count();
                if let Err(e) = d.check(id, &r.path) {
                    failures.push(e);
                }
            }
            // Hold-based presets keep their notes until the next press: that is their job.
            let held = p.info().params.iter().find(|i| i.name == "Hold").is_some_and(|i| preset.params.get(&i.id.to_string()).copied().unwrap_or(0.0) > 0.5);
            if held {
                p.reset();
                d.open.clear();
            }
            d.open.retain(|_, c| *c != 0);
            if ons == 0 {
                failures.push(format!("{}: no notes", r.path));
            }
            if !d.open.is_empty() {
                failures.push(format!("{}: notes left open {:?}", r.path, d.open));
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// `cargo test -p aitrack-stock --release --test midi_fx -- --ignored --nocapture`
///
/// Cost of `process_midi` at 48 kHz in 128-frame blocks under load: every
/// block releases a key and presses another (so the arpeggiator also sees a
/// changing chord), the arpeggiator at its fastest rate (1/32T) and the chord
/// device with its six voices on.
#[test]
#[ignore]
fn cpu_per_midi_device() {
    let seconds = 20.0;
    let block = 128usize;
    let blocks = (seconds * SR / block as f64) as usize;
    println!("\n{:<16} {:>10} {:>14}", "device", "CPU %", "ns per block");
    for id in MIDI_IDS {
        let mut p = aitrack_stock::create(id).unwrap();
        p.prepare(SR, 4096);
        for info in p.info().params.clone() {
            match (*id, info.name) {
                ("midi.arp", "Rate") => p.set_param(info.id, 1.0),
                ("midi.arp", "Gate") => p.set_param(info.id, 0.5),
                ("midi.chord", n) if n.starts_with("Voice") => p.set_param(info.id, 1.0),
                _ => {}
            }
        }
        let mut d = Driver::new(block);
        let mut events: Vec<Event> = Vec::with_capacity(4);
        let t = std::time::Instant::now();
        for b in 0..blocks {
            events.clear();
            let key = 48 + (b % 24) as u8;
            let prev = 48 + (b.wrapping_sub(4) % 24) as u8;
            if b >= 4 {
                events.push(Event { offset: 0, kind: EventKind::NoteOff { channel: 0, key: prev, velocity: 0.0, note_id: prev as i32 } });
            }
            events.push(Event { offset: 1, kind: EventKind::NoteOn { channel: 0, key, velocity: 0.8, note_id: key as i32 } });
            d.out.clear();
            let ctx = MidiContext { sample_rate: SR, frames: block, events: &events, transport: &d.transport };
            p.process_midi(&ctx, &mut EventSink::new(&mut d.out, &mut d.dropped));
            d.transport.position_samples += block as i64;
            d.transport.position_beats += block as f64 / SR * d.transport.bpm / 60.0;
        }
        let el = t.elapsed().as_secs_f64();
        println!("{:<16} {:>10.5} {:>14.0}   (dropped {})", id, 100.0 * el / seconds, el * 1e9 / blocks as f64, d.dropped);
    }
}
