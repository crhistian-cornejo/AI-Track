//! The MIDI effect framework: a `Category::MidiEffect` processor turns the
//! events of its chain into new ones before the instrument sees them. The
//! devices here are test devices (a transposer, an echo that repeats every
//! note a sixteenth later, a flood and a tiny-queue echo), not stock ones.

use crate::export::render_offline;
use crate::factory::ProcessorFactory;
use crate::fallback::{TEST_LATENCY, TEST_SYNTH};
use crate::{AudioProcessor, Engine, EngineConfig, EngineHandle, MidiMessage};
use aitrack_dsp::{AudioBuffer, Category, EventKind, EventQueue, EventSink, MidiContext, ParamInfo, PluginInfo, ProcessContext, Processor};
use aitrack_model::*;
use std::sync::{Arc, Mutex};

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;
const BEAT: usize = 24_000;
/// A sixteenth note at 120 BPM.
const SIXTEENTH: usize = BEAT / 4;

/// `(device, is note-on, key, position)`; an all-notes-off is a note-off of key 255.
type Logged = (u64, bool, u8, i64);

const KEYS: &str = "probe.keys";
const PITCH: &str = "t.pitch";
const ECHO: &str = "t.echo16";
const TINY_ECHO: &str = "t.echo.tiny";
const FLOOD: &str = "t.flood";

// ───────────────────────────── Test devices ─────────────────────────────

fn info(id: &'static str, category: Category) -> PluginInfo {
    PluginInfo {
        id,
        name: id,
        vendor: "AI-Track",
        category,
        tags: &[],
        params: vec![ParamInfo::new(0, "Amount", 0.0, 1.0, 0.5)],
        inputs: 2,
        outputs: 2,
        has_sidechain: false,
    }
}

/// Records the notes it receives with their position on the timeline, and
/// leaves the audio alone.
struct Keys {
    info: PluginInfo,
    id: u64,
    log: Arc<Mutex<Vec<Logged>>>,
}

impl Processor for Keys {
    fn info(&self) -> &PluginInfo {
        &self.info
    }
    fn prepare(&mut self, _: f64, _: usize) {}
    fn reset(&mut self) {}
    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        let mut log = self.log.lock().unwrap();
        for e in ctx.events {
            let at = ctx.transport.position_samples + e.offset as i64;
            match e.kind {
                EventKind::NoteOn { key, .. } => log.push((self.id, true, key, at)),
                EventKind::NoteOff { key, .. } => log.push((self.id, false, key, at)),
                EventKind::AllNotesOff => log.push((self.id, false, 255, at)),
                _ => {}
            }
        }
    }
    fn set_param(&mut self, _: u32, _: f32) {}
    fn get_param(&self, _: u32) -> f32 {
        0.0
    }
}

/// Transposes by `round(amount * 96 - 48)` semitones (0.625 = +12).
struct Pitch {
    info: PluginInfo,
    amount: f32,
}

impl Processor for Pitch {
    fn info(&self) -> &PluginInfo {
        &self.info
    }
    fn prepare(&mut self, _: f64, _: usize) {}
    fn reset(&mut self) {}
    fn process(&mut self, _: &mut ProcessContext<'_>) {
        panic!("a MIDI effect is never asked for audio");
    }
    fn set_param(&mut self, id: u32, v: f32) {
        if id == 0 {
            self.amount = v;
        }
    }
    fn get_param(&self, _: u32) -> f32 {
        self.amount
    }
    fn process_midi(&mut self, ctx: &MidiContext<'_>, out: &mut EventSink<'_>) {
        let st = (self.amount * 96.0 - 48.0).round() as i32;
        let shift = |key: u8| (key as i32 + st).clamp(0, 127) as u8;
        for e in ctx.events {
            let kind = match e.kind {
                EventKind::NoteOn { channel, key, velocity, note_id } => EventKind::NoteOn { channel, key: shift(key), velocity, note_id },
                EventKind::NoteOff { channel, key, velocity, note_id } => EventKind::NoteOff { channel, key: shift(key), velocity, note_id },
                k => k,
            };
            out.push(e.offset, kind);
        }
    }
}

/// Plays every note again a sixteenth later (note-on and note-off both),
/// through a queue of `capacity` pending events.
struct Echo {
    info: PluginInfo,
    queue: EventQueue,
}

impl Processor for Echo {
    fn info(&self) -> &PluginInfo {
        &self.info
    }
    fn prepare(&mut self, _: f64, _: usize) {}
    fn reset(&mut self) {
        self.queue.clear();
    }
    fn process(&mut self, _: &mut ProcessContext<'_>) {
        panic!("a MIDI effect is never asked for audio");
    }
    fn set_param(&mut self, _: u32, _: f32) {}
    fn get_param(&self, _: u32) -> f32 {
        0.0
    }
    fn process_midi(&mut self, ctx: &MidiContext<'_>, out: &mut EventSink<'_>) {
        let delay = (0.25 * 60.0 / ctx.transport.bpm * ctx.sample_rate).round() as i64;
        for e in ctx.events {
            out.push(e.offset, e.kind);
            if matches!(e.kind, EventKind::NoteOn { .. } | EventKind::NoteOff { .. }) {
                self.queue.schedule(e.offset, delay, e.kind);
            }
        }
        self.queue.drain_due(ctx.frames, out);
    }
    fn dropped_events(&self) -> u64 {
        self.queue.dropped()
    }
}

/// Writes 3000 notes per block: more than a block's event list holds.
struct Flood {
    info: PluginInfo,
}

impl Processor for Flood {
    fn info(&self) -> &PluginInfo {
        &self.info
    }
    fn prepare(&mut self, _: f64, _: usize) {}
    fn reset(&mut self) {}
    fn process(&mut self, _: &mut ProcessContext<'_>) {}
    fn set_param(&mut self, _: u32, _: f32) {}
    fn get_param(&self, _: u32) -> f32 {
        0.0
    }
    fn process_midi(&mut self, ctx: &MidiContext<'_>, out: &mut EventSink<'_>) {
        for i in 0..3_000 {
            out.push((i % ctx.frames) as u32, EventKind::NoteOn { channel: 0, key: (i % 128) as u8, velocity: 0.5, note_id: i as i32 });
        }
    }
}

#[derive(Default)]
struct Fx {
    log: Arc<Mutex<Vec<Logged>>>,
}

impl ProcessorFactory for Fx {
    fn create(&self, device: &Device, _: f64, _: usize) -> Option<Box<dyn Processor>> {
        let DeviceKind::Stock { plugin_id } = &device.kind else { return None };
        Some(match plugin_id.as_str() {
            KEYS => Box::new(Keys { info: info(KEYS, Category::Effect), id: device.id.0, log: self.log.clone() }),
            PITCH => Box::new(Pitch { info: info(PITCH, Category::MidiEffect), amount: 0.5 }),
            ECHO => Box::new(Echo { info: info(ECHO, Category::MidiEffect), queue: EventQueue::with_capacity(64) }),
            TINY_ECHO => Box::new(Echo { info: info(TINY_ECHO, Category::MidiEffect), queue: EventQueue::with_capacity(2) }),
            FLOOD => Box::new(Flood { info: info(FLOOD, Category::MidiEffect) }),
            _ => return None,
        })
    }
}

impl Fx {
    /// `(is note-on, key, position)` the probe `device` received.
    fn notes(&self, device: DeviceId) -> Vec<(bool, u8, i64)> {
        self.log.lock().unwrap().iter().filter(|n| n.0 == device.0).map(|n| (n.1, n.2, n.3)).collect()
    }
}

// ───────────────────────────── Builders ─────────────────────────────

struct B {
    p: Project,
    track: TrackId,
}

impl B {
    fn new() -> Self {
        let mut p = Project::new("midi fx");
        let track = p.alloc_track_id();
        p.tracks.push(Track::new(track, TrackKind::Instrument, "t", Color(0)));
        Self { p, track }
    }

    fn device(&mut self, plugin: &str, params: &[(u32, f32)]) -> DeviceId {
        let id = self.p.alloc_device_id();
        let mut d = Device::stock(id, plugin, plugin);
        for &(k, v) in params {
            d.params.insert(k, v);
        }
        self.p.track_mut(self.track).unwrap().devices.push(d);
        id
    }

    fn notes(&mut self, notes: &[(u8, Tick, Tick)]) {
        let sid = self.p.alloc_source_id();
        let mut src = NoteSource::default();
        for &(key, start, len) in notes {
            let nid = self.p.alloc_note_id();
            src.notes.push(Note::new(nid, key, start, len, 1.0));
        }
        self.p.sources.insert(sid, Source::Notes(src));
        let cid = self.p.alloc_clip_id();
        self.p.clips.insert(cid, Clip::new("c", Color(0), sid, 4 * PPQ));
        let pid = self.p.alloc_placement_id();
        self.p.track_mut(self.track).unwrap().arrangement.push(Placement { id: pid, clip: cid, start: 0, lane: 0 });
    }

    fn render(&self, f: &dyn ProcessorFactory, block: usize, ticks: Tick) -> AudioBuffer {
        render_offline(&self.p, f, (0, ticks), SR, block).unwrap()
    }
}

fn start(p: &Project, f: Arc<dyn ProcessorFactory>) -> (EngineHandle, AudioProcessor) {
    let (h, proc_) = Engine::new(EngineConfig::headless(SR), f).unwrap();
    h.update_project(p);
    h.flush();
    let mut proc_ = proc_.unwrap();
    let mut buf = AudioBuffer::new(2, BLOCK);
    proc_.process_planar(&mut buf);
    (h, proc_)
}

fn blocks(proc_: &mut AudioProcessor, n: usize) {
    let mut buf = AudioBuffer::new(2, BLOCK);
    for _ in 0..n {
        proc_.process_planar(&mut buf);
    }
}

// ───────────────────────────── The tests ─────────────────────────────

#[test]
fn a_pitch_effect_in_front_of_a_synth_transposes_live_and_in_export() {
    // The effect is placed AFTER the synth in the chain; it still runs first.
    let mut b = B::new();
    let synth = b.device(TEST_SYNTH, &[(0, 1.0)]);
    let _ = synth;
    b.device(PITCH, &[(0, 0.625)]);
    b.notes(&[(60, 0, PPQ)]);
    // The same note an octave up, played by the synth alone.
    let mut reference = B::new();
    reference.device(TEST_SYNTH, &[(0, 1.0)]);
    reference.notes(&[(72, 0, PPQ)]);
    let f = Fx::default();
    let (got, want) = (b.render(&f, 256, 2 * PPQ), reference.render(&f, 256, 2 * PPQ));
    assert!(got.channel(0).iter().any(|s| s.abs() > 0.01), "it sounds");
    assert_eq!(got.channel(0), want.channel(0), "+12 on the way in is the note an octave up");
    // Block size does not matter.
    assert_eq!(b.render(&f, 1024, 2 * PPQ).channel(0), want.channel(0));

    // Live: the probe after the effect gets 72 for the 60 that was played.
    let mut live = B::new();
    live.device(PITCH, &[(0, 0.625)]);
    let probe = live.device(KEYS, &[]);
    let track = live.track;
    let f = Arc::new(Fx::default());
    let (h, mut proc_) = start(&live.p, f.clone());
    h.send_midi_to(track, MidiMessage::note_on(0, 60, 100));
    blocks(&mut proc_, 2);
    h.send_midi_to(track, MidiMessage::note_off(0, 60));
    blocks(&mut proc_, 2);
    assert_eq!(f.notes(probe).iter().map(|n| (n.0, n.1)).collect::<Vec<_>>(), vec![(true, 72), (false, 72)]);
}

#[test]
fn effects_chain_in_order_and_each_gets_the_previous_output() {
    let mut b = B::new();
    b.device(PITCH, &[(0, 0.625)]); // +12
    b.device(PITCH, &[(0, 0.5 + 7.0 / 96.0)]); // +7
    let probe = b.device(KEYS, &[]);
    let track = b.track;
    let f = Arc::new(Fx::default());
    let (h, mut proc_) = start(&b.p, f.clone());
    h.send_midi_to(track, MidiMessage::note_on(0, 40, 100));
    blocks(&mut proc_, 2);
    assert_eq!(f.notes(probe).iter().map(|n| n.1).collect::<Vec<_>>(), vec![59]);
}

#[test]
fn an_effect_inside_a_rack_chain_acts_on_that_chain_only() {
    let mut b = B::new();
    let id = b.p.alloc_device_id();
    let (ka, kb, fx) = (b.p.alloc_device_id(), b.p.alloc_device_id(), b.p.alloc_device_id());
    let mut rack = Rack::new(RackKind::Instrument);
    let mut a = Chain::new("a", Color(0));
    a.devices.push(Device::stock(ka, KEYS, KEYS));
    let mut pitch = Device::stock(fx, PITCH, PITCH);
    pitch.params.insert(0, 0.625);
    a.devices.push(pitch);
    let mut c = Chain::new("b", Color(0));
    c.devices.push(Device::stock(kb, KEYS, KEYS));
    rack.chains.push(a);
    rack.chains.push(c);
    b.p.track_mut(b.track).unwrap().devices.push(Device { kind: DeviceKind::Rack(rack), ..Device::stock(id, "rack", "Rack") });
    let track = b.track;
    let f = Arc::new(Fx::default());
    let (h, mut proc_) = start(&b.p, f.clone());
    h.send_midi_to(track, MidiMessage::note_on(0, 50, 100));
    blocks(&mut proc_, 2);
    assert_eq!(f.notes(ka).iter().map(|n| n.1).collect::<Vec<_>>(), vec![62], "transposed in its own chain");
    assert_eq!(f.notes(kb).iter().map(|n| n.1).collect::<Vec<_>>(), vec![50], "the other chain keeps the played note");
}

#[test]
fn a_delayed_copy_comes_out_blocks_later_and_offline_matches_live() {
    let mut b = B::new();
    b.device(ECHO, &[]);
    let probe = b.device(KEYS, &[]);
    b.notes(&[(60, 0, PPQ / 2)]);
    // Offline, in two block sizes.
    for block in [256, 512] {
        let f = Fx::default();
        b.render(&f, block, PPQ);
        let got = f.notes(probe);
        let len = (BEAT / 2) as i64;
        assert_eq!(
            got,
            vec![(true, 60, 0), (true, 60, SIXTEENTH as i64), (false, 60, len), (false, 60, len + SIXTEENTH as i64)],
            "block {block}"
        );
    }
    // Live: the note off the clip's schedule comes from the transport too.
    let f = Arc::new(Fx::default());
    let (h, mut proc_) = start(&b.p, f.clone());
    h.play();
    blocks(&mut proc_, 100);
    h.stop();
    let got = f.notes(probe);
    assert_eq!(got[..2], [(true, 60, 0), (true, 60, SIXTEENTH as i64)], "{got:?}");
}

#[test]
fn a_disabled_effect_lets_events_through_untouched() {
    let mut b = B::new();
    let fx = b.device(PITCH, &[(0, 0.625)]);
    let probe = b.device(KEYS, &[]);
    b.p.track_mut(b.track).unwrap().devices.iter_mut().find(|d| d.id == fx).unwrap().enabled = false;
    let track = b.track;
    let f = Arc::new(Fx::default());
    let (h, mut proc_) = start(&b.p, f.clone());
    h.send_midi_to(track, MidiMessage::note_on(0, 60, 100));
    blocks(&mut proc_, 2);
    assert_eq!(f.notes(probe).iter().map(|n| n.1).collect::<Vec<_>>(), vec![60]);
}

#[test]
fn midi_effects_add_no_audio_latency() {
    let mut b = B::new();
    b.device(PITCH, &[]);
    b.device(ECHO, &[]);
    let id = b.p.alloc_device_id();
    let mut lat = Device::stock(id, TEST_LATENCY, TEST_LATENCY);
    lat.state = Some(300u32.to_le_bytes().to_vec());
    b.p.track_mut(b.track).unwrap().devices.push(lat);
    let g = crate::compile::compile(&b.p, &mut crate::compile::CompileContext {
        factory: &Fx::default(),
        pool: &mut crate::shared::ProcessorPool::new(),
        samples: &crate::sample_cache::SampleCache::blocking(),
        sample_rate: SR,
        max_frames: BLOCK,
        prev_meters: None,
        taps: Vec::new(),
        analysis: None,
    })
    .unwrap();
    assert_eq!(g.total_latency(), 300, "only the audio device counts");
}

#[test]
fn a_full_queue_and_a_full_event_list_drop_and_count() {
    // The echo's queue holds two: a third note in a row is refused.
    let mut b = B::new();
    b.device(TINY_ECHO, &[]);
    b.device(FLOOD, &[]);
    let track = b.track;
    let (h, mut proc_) = start(&b.p, Arc::new(Fx::default()));
    for key in 60..64 {
        h.send_midi_to(track, MidiMessage::note_on(0, key, 100));
    }
    blocks(&mut proc_, 3);
    let dropped = h.audio_health().midi_events_dropped;
    // 3 000 flood events per block into a list of 2 048, plus the queue's.
    assert!(dropped >= 2 * (3_000 - 2_048), "{dropped}");
    // The engine itself did not choke.
    assert_eq!(h.audio_health().callback_panics, 0);
}

fn held_echo() -> (B, DeviceId) {
    let mut b = B::new();
    b.device(ECHO, &[]);
    let probe = b.device(KEYS, &[]);
    (b, probe)
}

/// Net note state per key from a probe log: the keys still sounding.
fn sounding(notes: &[(bool, u8, i64)]) -> Vec<u8> {
    let mut on: Vec<u8> = Vec::new();
    for (is_on, key, _) in notes {
        if *is_on {
            on.push(*key);
        } else if *key == 255 {
            on.clear();
        } else {
            on.retain(|k| k != key);
        }
    }
    on
}

#[test]
fn stopping_the_transport_closes_what_the_effect_left_open() {
    let (b, probe) = held_echo();
    let track = b.track;
    let f = Arc::new(Fx::default());
    let (h, mut proc_) = start(&b.p, f.clone());
    h.send_midi_to(track, MidiMessage::note_on(0, 60, 100));
    // Ten blocks in: the echo of the note (due at block 24) is still pending
    // and the note is held.
    blocks(&mut proc_, 10);
    assert_eq!(f.notes(probe).len(), 1, "only the played note so far");
    h.stop();
    blocks(&mut proc_, 40);
    let got = f.notes(probe);
    assert!(sounding(&got).is_empty(), "the held note was closed: {got:?}");
    assert_eq!(got.iter().filter(|n| n.0).count(), 1, "and the pending echo never came out: {got:?}");
}

#[test]
fn a_seek_closes_pending_notes() {
    let (b, probe) = held_echo();
    let track = b.track;
    let f = Arc::new(Fx::default());
    let (h, mut proc_) = start(&b.p, f.clone());
    h.play();
    h.send_midi_to(track, MidiMessage::note_on(0, 62, 100));
    blocks(&mut proc_, 2);
    h.seek(PPQ); // the echo of the note-on was still pending
    blocks(&mut proc_, 40);
    let got = f.notes(probe);
    assert_eq!(got.iter().filter(|n| n.0).count(), 1, "the pending echo was dropped by the jump: {got:?}");
    assert!(sounding(&got).is_empty(), "the held note was closed: {got:?}");
}

#[test]
fn switching_an_effect_off_closes_its_notes_and_stops_the_echo() {
    let (mut b, probe) = held_echo();
    let fx = b.p.track(b.track).unwrap().devices[0].id;
    // Enabled until a beat in, off after.
    let mut lane = AutomationLane::new(AutomationTarget::DeviceEnabled { device: fx });
    lane.points = vec![AutomationPoint { tick: 0, value: 1.0, curve: 0.0 }, AutomationPoint { tick: PPQ, value: 1.0, curve: 0.0 }, AutomationPoint { tick: PPQ + 1, value: 0.0, curve: 0.0 }];
    b.p.track_mut(b.track).unwrap().automation.push(lane);
    let track = b.track;
    let f = Arc::new(Fx::default());
    let (h, mut proc_) = start(&b.p, f.clone());
    h.play();
    h.send_midi_to(track, MidiMessage::note_on(0, 64, 100));
    // 1.5 beats: past the switch-off, with the note held.
    blocks(&mut proc_, (3 * BEAT / 2) / BLOCK);
    let got = f.notes(probe);
    assert_eq!(got.iter().filter(|n| n.0).count(), 2, "played note and echo: {got:?}");
    assert!(sounding(&got).is_empty(), "closed when the effect went off: {got:?}");
    // From now on notes go straight through, with no echo.
    h.send_midi_to(track, MidiMessage::note_on(0, 65, 100));
    blocks(&mut proc_, 40);
    let got = f.notes(probe);
    assert_eq!(got.iter().filter(|n| n.0 && n.1 == 65).count(), 1, "no echo while off: {got:?}");
}

#[test]
fn recompiling_closes_the_notes_an_effect_had_open() {
    let (mut b, probe) = held_echo();
    let track = b.track;
    let f = Arc::new(Fx::default());
    let (h, mut proc_) = start(&b.p, f.clone());
    h.send_midi_to(track, MidiMessage::note_on(0, 66, 100));
    blocks(&mut proc_, 2);
    assert!(sounding(&f.notes(probe)).contains(&66));
    // A structural edit: a new graph.
    b.p.track_mut(track).unwrap().mute = true;
    h.update_project(&b.p);
    h.flush();
    blocks(&mut proc_, 40);
    let got = f.notes(probe);
    assert!(sounding(&got).is_empty(), "the new graph closed it: {got:?}");
    assert_eq!(got.iter().filter(|n| n.0).count(), 1, "and the echo that was pending never came: {got:?}");
}

#[test]
fn removing_an_effect_that_held_notes_silences_the_chain() {
    let (mut b, probe) = held_echo();
    let track = b.track;
    let f = Arc::new(Fx::default());
    let (h, mut proc_) = start(&b.p, f.clone());
    h.send_midi_to(track, MidiMessage::note_on(0, 67, 100));
    blocks(&mut proc_, 2);
    assert!(sounding(&f.notes(probe)).contains(&67));
    // The effect is deleted from the project: nothing is left to close the
    // note, so the chain gets an all-notes-off.
    b.p.track_mut(track).unwrap().devices.remove(0);
    h.update_project(&b.p);
    h.flush();
    blocks(&mut proc_, 3);
    assert!(sounding(&f.notes(probe)).is_empty(), "{:?}", f.notes(probe));
}

#[test]
fn midi_effects_never_allocate_on_the_audio_thread() {
    let mut b = B::new();
    b.device(PITCH, &[(0, 0.625)]);
    b.device(ECHO, &[]);
    b.device(TEST_SYNTH, &[]);
    let track = b.track;
    let (h, mut proc_) = start(&b.p, Arc::new(Fx::default()));
    h.play();
    blocks(&mut proc_, 4);
    let mut buf = AudioBuffer::new(2, BLOCK);
    let ((), allocs) = crate::alloc_counter::count_allocs(|| {
        for i in 0..400u32 {
            let key = 48 + (i % 24) as u8;
            h.send_midi_to(track, MidiMessage::note_on(0, key, 90));
            h.send_midi_to(track, MidiMessage::note_off(0, key));
            proc_.process_planar(&mut buf);
            if i == 200 {
                h.stop();
            }
            if i == 210 {
                h.play();
            }
        }
    });
    assert_eq!(allocs, 0, "effects, delayed events, stop and restart");
}

// ───────────────────────────── MIDI Effect Rack ─────────────────────────────

/// A MIDI Effect Rack on the track with one chain per `(key zone, [(effect, amount)])`.
/// A chain of a test rack: its key zone and its effects with their first parameter.
type TestChain<'a> = ((u8, u8), &'a [(&'a str, f32)]);

fn midi_rack(b: &mut B, chains: &[TestChain<'_>]) -> DeviceId {
    let id = b.p.alloc_device_id();
    let mut rack = Rack::new(RackKind::Midi);
    for (i, (zone, effects)) in chains.iter().enumerate() {
        let mut c = Chain::new(format!("chain {i}"), Color(0));
        c.key_range = *zone;
        for (plugin, amount) in effects.iter() {
            let d = b.p.alloc_device_id();
            let mut dev = Device::stock(d, *plugin, *plugin);
            dev.params.insert(0, *amount);
            c.devices.push(dev);
        }
        rack.chains.push(c);
    }
    b.p.track_mut(b.track).unwrap().devices.push(Device { kind: DeviceKind::Rack(rack), ..Device::stock(id, "rack.midi", "MIDI Effect Rack") });
    id
}

#[test]
fn parallel_chains_of_a_midi_rack_are_merged_in_front_of_the_instrument() {
    let mut b = B::new();
    // The probe is placed before the rack; the rack still runs first.
    let probe = b.device(KEYS, &[]);
    midi_rack(&mut b, &[((0, 127), &[(PITCH, 0.625)]), ((0, 127), &[(PITCH, 0.5 + 7.0 / 96.0)])]);
    let track = b.track;
    let f = Arc::new(Fx::default());
    let (h, mut proc_) = start(&b.p, f.clone());
    h.send_midi_to(track, MidiMessage::note_on(0, 60, 100));
    blocks(&mut proc_, 2);
    h.send_midi_to(track, MidiMessage::note_off(0, 60));
    blocks(&mut proc_, 2);
    let mut ons: Vec<u8> = f.notes(probe).iter().filter(|n| n.0).map(|n| n.1).collect();
    ons.sort();
    assert_eq!(ons, vec![67, 72], "+7 and +12, both");
    assert_eq!(f.notes(probe).iter().filter(|n| !n.0).count(), 2, "and both end");
}

#[test]
fn a_midi_rack_routes_by_key_zone_and_chain_select() {
    let mut b = B::new();
    let rack = midi_rack(&mut b, &[((0, 59), &[(PITCH, 0.625)]), ((60, 127), &[(PITCH, 0.5 + 7.0 / 96.0)])]);
    let probe = b.device(KEYS, &[]);
    // The second chain is only open for chain-select values 64..=127.
    if let Some(DeviceKind::Rack(r)) = b.p.track_mut(b.track).unwrap().devices.iter_mut().find(|d| d.id == rack).map(|d| &mut d.kind) {
        r.chains[1].selector_range = (64, 127);
        r.chain_selector = 0;
    }
    let track = b.track;
    let f = Arc::new(Fx::default());
    let (h, mut proc_) = start(&b.p, f.clone());
    h.send_midi_to(track, MidiMessage::note_on(0, 40, 100));
    h.send_midi_to(track, MidiMessage::note_on(0, 62, 100));
    blocks(&mut proc_, 2);
    assert_eq!(f.notes(probe).iter().map(|n| n.1).collect::<Vec<_>>(), vec![52], "62 is in the closed chain's zone: only 40 -> 52 sounds");

    // Open the selector: now 62 passes through its chain (+7).
    let DeviceKind::Rack(r) = &mut b.p.track_mut(b.track).unwrap().devices.iter_mut().find(|d| d.id == rack).unwrap().kind else { panic!() };
    r.chain_selector = 100;
    let (h, mut proc_) = start(&b.p, f.clone());
    f.log.lock().unwrap().clear();
    h.send_midi_to(track, MidiMessage::note_on(0, 62, 100));
    blocks(&mut proc_, 2);
    assert_eq!(f.notes(probe).iter().map(|n| n.1).collect::<Vec<_>>(), vec![69]);
}

#[test]
fn a_disabled_midi_rack_is_transparent() {
    let mut b = B::new();
    let rack = midi_rack(&mut b, &[((0, 127), &[(PITCH, 0.625)])]);
    let probe = b.device(KEYS, &[]);
    b.p.track_mut(b.track).unwrap().devices.iter_mut().find(|d| d.id == rack).unwrap().enabled = false;
    let track = b.track;
    let f = Arc::new(Fx::default());
    let (h, mut proc_) = start(&b.p, f.clone());
    h.send_midi_to(track, MidiMessage::note_on(0, 60, 100));
    blocks(&mut proc_, 2);
    assert_eq!(f.notes(probe).iter().map(|n| n.1).collect::<Vec<_>>(), vec![60]);
}

#[test]
fn a_midi_rack_sounds_the_same_in_any_block_size_and_never_allocates_live() {
    let mut b = B::new();
    midi_rack(&mut b, &[((0, 127), &[(PITCH, 0.625)]), ((0, 127), &[(ECHO, 0.0)])]);
    b.device(TEST_SYNTH, &[(0, 1.0)]);
    b.notes(&[(60, 0, PPQ)]);
    let f = Fx::default();
    let a = b.render(&f, 256, 2 * PPQ);
    assert!(a.channel(0).iter().any(|s| s.abs() > 0.01), "it sounds");
    assert_eq!(a.channel(0), b.render(&f, 1024, 2 * PPQ).channel(0), "block size does not matter");

    let track = b.track;
    let (h, mut proc_) = start(&b.p, Arc::new(Fx::default()));
    h.play();
    blocks(&mut proc_, 4);
    let mut buf = AudioBuffer::new(2, BLOCK);
    let ((), allocs) = crate::alloc_counter::count_allocs(|| {
        for i in 0..300u32 {
            let key = 48 + (i % 24) as u8;
            h.send_midi_to(track, MidiMessage::note_on(0, key, 90));
            h.send_midi_to(track, MidiMessage::note_off(0, key));
            proc_.process_planar(&mut buf);
        }
    });
    assert_eq!(allocs, 0);
}
