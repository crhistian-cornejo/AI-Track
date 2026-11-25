//! Every MIDI effect of the registry: `process_midi` never touches the heap,
//! writes sorted events inside the block, and (for the devices of the
//! timing/pitch family and the chord) closes every note it opens, under every factory
//! preset and under extreme and scrambled parameters.

use aitrack_dsp::{Category, Event, EventKind, EventSink, MidiContext, Processor, Transport};
use aitrack_stock::presets;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};

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

const SR: f64 = 48_000.0;
const BLOCK: usize = 128;
/// The devices this file checks note by note: the timing/pitch family, and
/// the chord (its strum delays notes by up to half a second).
const PAIRED: [&str; 5] = ["midi.echo", "midi.random", "midi.length", "midi.pitch", "midi.chord"];

fn midi_effects() -> Vec<&'static str> {
    aitrack_stock::registry().iter().filter(|e| e.category == Category::MidiEffect).map(|e| e.id).collect()
}

/// A deterministic stream of well-formed notes (a key is never pressed twice
/// before its release), a chord now and then, plus the odd controller.
struct Feed {
    seed: u32,
    /// Block index at which each key is released (0 = free).
    release_at: [u64; 128],
    /// A key released in a block is pressed again no sooner than the next.
    free_from: [u64; 128],
    block: u64,
    /// Stop pressing new notes after this block.
    until: u64,
}

impl Feed {
    fn new(seed: u32, until: u64) -> Self {
        Self { seed, release_at: [0; 128], free_from: [0; 128], block: 0, until }
    }

    fn rnd(&mut self) -> u32 {
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 17;
        self.seed ^= self.seed << 5;
        self.seed
    }

    /// Fill `events` for the next block.
    fn next(&mut self, events: &mut Vec<Event>) {
        events.clear();
        let b = self.block;
        for key in 0..128u8 {
            if self.release_at[key as usize] != 0 && self.release_at[key as usize] <= b {
                events.push(Event { offset: self.rnd() % BLOCK as u32, kind: EventKind::NoteOff { channel: 0, key, velocity: 0.0, note_id: key as i32 } });
                self.release_at[key as usize] = 0;
                self.free_from[key as usize] = b + 1;
            }
        }
        if b < self.until && self.rnd().is_multiple_of(3) {
            for _ in 0..1 + self.rnd() % 3 {
                let key = 30 + (self.rnd() % 50) as u8;
                if self.release_at[key as usize] == 0 && self.free_from[key as usize] <= b {
                    let vel = 0.2 + (self.rnd() % 80) as f32 / 100.0;
                    events.push(Event { offset: self.rnd() % BLOCK as u32, kind: EventKind::NoteOn { channel: 0, key, velocity: vel, note_id: key as i32 } });
                    self.release_at[key as usize] = b + 1 + (self.rnd() % 400) as u64;
                }
            }
        }
        if b % 50 == 7 {
            events.push(Event { offset: 3, kind: EventKind::ControlChange { channel: 0, cc: 1, value: 0.5 } });
            events.push(Event { offset: 4, kind: EventKind::PitchBend { channel: 0, value: 0.1 } });
        }
        // Per key, a release must come after its press in the same block.
        events.sort_by_key(|e| (e.offset, matches!(e.kind, EventKind::NoteOn { .. })));
        self.block += 1;
    }
}

fn param_passes(p: &dyn Processor) -> Vec<Vec<f32>> {
    let n = p.info().params.len();
    (0..5)
        .map(|pass| {
            (0..n)
                .map(|j| match pass {
                    0 => p.info().params[j].default_normalized(),
                    1 => 0.0,
                    2 => 1.0,
                    3 => ((j * 7919 + 13) % 101) as f32 / 100.0,
                    _ => ((j * 104_729 + 7) % 97) as f32 / 96.0,
                })
                .collect()
        })
        .collect()
}

fn set_all(p: &mut dyn Processor, values: &[f32]) {
    let ids: Vec<u32> = p.info().params.iter().map(|i| i.id).collect();
    for (id, v) in ids.iter().zip(values) {
        p.set_param(*id, *v);
    }
}

fn factory(id: &str) -> Vec<presets::Preset> {
    presets::list(id).iter().map(|r| presets::load(&r.path).unwrap()).collect()
}

#[test]
fn process_midi_never_touches_the_heap() {
    let mut failures = Vec::new();
    for id in midi_effects() {
        let mut p = aitrack_stock::create(id).unwrap();
        p.prepare(SR, 4096);
        let mut out: Vec<Event> = Vec::with_capacity(2048);
        let mut events: Vec<Event> = Vec::with_capacity(64);
        let mut tr = Transport { playing: true, bpm: 128.0, ..Transport::default() };
        let mut settings = param_passes(p.as_ref());
        // Every factory preset too, applied the way the engine does: plain `set_param` calls.
        for preset in factory(id) {
            let values: Vec<f32> = p.info().params.iter().map(|i| preset.params.get(&i.id.to_string()).copied().unwrap_or(i.default_normalized())).collect();
            settings.push(values);
        }
        for (s, values) in settings.iter().enumerate() {
            set_all(p.as_mut(), values);
            let mut feed = Feed::new(0x1234 + s as u32, 150);
            let mut dropped = 0u32;
            let n = counted(|| {
                for _ in 0..200 {
                    feed.next(&mut events);
                    out.clear();
                    let ctx = MidiContext { sample_rate: SR, frames: BLOCK, events: &events, transport: &tr };
                    p.process_midi(&ctx, &mut EventSink::new(&mut out, &mut dropped));
                    tr.position_samples += BLOCK as i64;
                    tr.position_beats += BLOCK as f64 / SR * tr.bpm / 60.0;
                }
            });
            if n > 0 {
                failures.push(format!("{id} setting {s}: {n} heap operations"));
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

#[test]
fn midi_effects_write_sorted_events_inside_the_block_for_any_block_size() {
    let mut failures = Vec::new();
    for id in midi_effects() {
        for block in [1usize, 5, 64, 777, 4096] {
            let mut p = aitrack_stock::create(id).unwrap();
            p.prepare(SR, 4096);
            let mut out: Vec<Event> = Vec::with_capacity(2048);
            let mut tr = Transport { playing: true, bpm: 128.0, ..Transport::default() };
            let mut feed = Feed::new(77, 20);
            let mut events = Vec::with_capacity(64);
            let mut dropped = 0u32;
            for _ in 0..(48_000 / block.max(128)).max(20) {
                // One feed block (128 frames) is delivered as a block of its own size when
                // larger, or split into pieces when smaller.
                feed.next(&mut events);
                let mut pos = 0usize;
                while pos < BLOCK {
                    let n = block.min(BLOCK - pos);
                    let piece: Vec<Event> = events.iter().filter(|e| (e.offset as usize) >= pos && (e.offset as usize) < pos + n).map(|e| Event { offset: e.offset - pos as u32, kind: e.kind }).collect();
                    out.clear();
                    let ctx = MidiContext { sample_rate: SR, frames: n, events: &piece, transport: &tr };
                    p.process_midi(&ctx, &mut EventSink::new(&mut out, &mut dropped));
                    let mut last = 0;
                    for e in &out {
                        if e.offset as usize >= n || e.offset < last {
                            failures.push(format!("{id} block {block}: offset {} (last {last}) in a block of {n}", e.offset));
                            break;
                        }
                        last = e.offset;
                    }
                    tr.position_samples += n as i64;
                    tr.position_beats += n as f64 / SR * tr.bpm / 60.0;
                    pos += n;
                }
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// Run `p` over a played passage and then in silence until everything has
/// died away; every note-on must have found its note-off.
fn run_and_check_pairs(id: &str, p: &mut dyn Processor, seed: u32, label: &str, failures: &mut Vec<String>) -> usize {
    let mut out: Vec<Event> = Vec::with_capacity(2048);
    let mut events: Vec<Event> = Vec::with_capacity(64);
    let mut tr = Transport { playing: true, bpm: 128.0, ..Transport::default() };
    let mut feed = Feed::new(seed, 300);
    let mut dropped = 0u32;
    let mut open: BTreeMap<(u8, u8), i32> = BTreeMap::new();
    let mut ons = 0usize;
    // 300 blocks of playing, then 70 s of silence (the longest echo or hold).
    let total = 300 + (70.0 * SR / BLOCK as f64) as u64;
    for b in 0..total {
        feed.next(&mut events);
        out.clear();
        let ctx = MidiContext { sample_rate: SR, frames: BLOCK, events: &events, transport: &tr };
        p.process_midi(&ctx, &mut EventSink::new(&mut out, &mut dropped));
        for e in &out {
            match e.kind {
                EventKind::NoteOn { channel, key, .. } => {
                    ons += 1;
                    let c = open.entry((channel, key)).or_default();
                    *c += 1;
                    if *c > 1 {
                        failures.push(format!("{id} {label}: two notes on key {key} at block {b}"));
                        return ons;
                    }
                }
                EventKind::NoteOff { channel, key, .. } => {
                    let c = open.entry((channel, key)).or_default();
                    *c -= 1;
                    if *c < 0 {
                        failures.push(format!("{id} {label}: note-off of key {key} without a note-on at block {b}"));
                        return ons;
                    }
                }
                EventKind::AllNotesOff => open.clear(),
                _ => {}
            }
        }
        tr.position_samples += BLOCK as i64;
        tr.position_beats += BLOCK as f64 / SR * tr.bpm / 60.0;
    }
    let left: Vec<_> = open.iter().filter(|(_, c)| **c != 0).collect();
    if !left.is_empty() {
        failures.push(format!("{id} {label}: notes left open {left:?}"));
    }
    ons
}

#[test]
fn every_note_on_gets_its_note_off_under_every_preset_and_extreme_settings() {
    let mut failures = Vec::new();
    for id in PAIRED {
        let mut p = aitrack_stock::create(id).unwrap();
        p.prepare(SR, 4096);
        for (s, values) in param_passes(p.as_ref()).iter().enumerate() {
            p.reset();
            set_all(p.as_mut(), values);
            run_and_check_pairs(id, p.as_mut(), 11 + s as u32, &format!("pass {s}"), &mut failures);
        }
        for preset in factory(id) {
            p.reset();
            let defaults: Vec<f32> = p.info().params.iter().map(|i| i.default_normalized()).collect();
            set_all(p.as_mut(), &defaults);
            preset.apply(p.as_mut());
            let ons = run_and_check_pairs(id, p.as_mut(), 5, &format!("preset {}", preset.name), &mut failures);
            if ons == 0 {
                failures.push(format!("{id} preset {}: no notes came out", preset.name));
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

/// `cargo test -p aitrack-stock --release --test midi_realtime -- --ignored --nocapture`
#[test]
#[ignore]
fn cpu_of_process_midi_per_device() {
    println!("\n{:<18} {:>9}  (48 kHz, {BLOCK} frames, dense feed)", "device", "CPU %");
    for id in midi_effects() {
        let mut p = aitrack_stock::create(id).unwrap();
        p.prepare(SR, 4096);
        let mut out: Vec<Event> = Vec::with_capacity(2048);
        let mut events: Vec<Event> = Vec::with_capacity(64);
        let mut tr = Transport { playing: true, bpm: 128.0, ..Transport::default() };
        let mut feed = Feed::new(3, u64::MAX);
        let mut dropped = 0u32;
        let blocks = (10.0 * SR / BLOCK as f64) as usize;
        let t = std::time::Instant::now();
        for _ in 0..blocks {
            feed.next(&mut events);
            out.clear();
            let ctx = MidiContext { sample_rate: SR, frames: BLOCK, events: &events, transport: &tr };
            p.process_midi(&ctx, &mut EventSink::new(&mut out, &mut dropped));
            tr.position_samples += BLOCK as i64;
            tr.position_beats += BLOCK as f64 / SR * tr.bpm / 60.0;
        }
        println!("{:<18} {:>9.4}", id, 100.0 * t.elapsed().as_secs_f64() / 10.0);
    }
}
