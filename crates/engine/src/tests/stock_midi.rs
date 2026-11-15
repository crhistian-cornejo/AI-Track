//! The real stock MIDI effects (`midi.*`, from `aitrack-stock`) through the
//! real engine: where their notes land, that live and offline agree, that
//! nothing is left sounding, that the model's order is honoured and that the
//! audio thread does not allocate.

use crate::alloc_counter::count_allocs;
use crate::export::render_offline;
use crate::factory::ProcessorFactory;
use crate::fallback::TEST_SYNTH;
use crate::{AudioProcessor, Engine, EngineConfig, EngineHandle, MidiMessage};
use aitrack_dsp::{AudioBuffer, Category, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor};
use aitrack_model::*;
use aitrack_stock::midi::{arp, echo, velocity};
use std::sync::{Arc, Mutex};

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;
const BEAT: i64 = 24_000;
/// `(device, is note-on, key, velocity 0..=127, position)`; an
/// all-notes-off is a note-off of key 255.
type Logged = (u64, bool, u8, u8, i64);
/// `(is note-on, key, velocity, position)` as one probe saw it.
type Seen = (bool, u8, u8, i64);

// ───────────────────────────── The probe and the factory ─────────────────────────────

const KEYS: &str = "probe.keys";

/// An audio-less listener after the effects: records the notes it gets and
/// where on the timeline.
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
                EventKind::NoteOn { key, velocity, .. } => log.push((self.id, true, key, (velocity * 127.0).round() as u8, at)),
                EventKind::NoteOff { key, .. } => log.push((self.id, false, key, 0, at)),
                EventKind::AllNotesOff => log.push((self.id, false, 255, 0, at)),
                _ => {}
            }
        }
    }
    fn set_param(&mut self, _: u32, _: f32) {}
    fn get_param(&self, _: u32) -> f32 {
        0.0
    }
}

/// The stock `midi.*` devices, the probe, and (through the engine's own
/// fallback) the test synth.
#[derive(Default)]
struct Stock {
    log: Arc<Mutex<Vec<Logged>>>,
}

impl ProcessorFactory for Stock {
    fn create(&self, device: &Device, _: f64, _: usize) -> Option<Box<dyn Processor>> {
        let DeviceKind::Stock { plugin_id } = &device.kind else { return None };
        if plugin_id == KEYS {
            let info = PluginInfo { id: KEYS, name: "Probe", vendor: "AI-Track", category: Category::Effect, tags: &[], params: vec![ParamInfo::new(0, "Unused", 0.0, 1.0, 0.0)], inputs: 2, outputs: 2, has_sidechain: false };
            return Some(Box::new(Keys { info, id: device.id.0, log: self.log.clone() }));
        }
        plugin_id.starts_with("midi.").then(|| aitrack_stock::create(plugin_id)).flatten()
    }
}

impl Stock {
    fn notes(&self, device: DeviceId) -> Vec<Seen> {
        self.log.lock().unwrap().iter().filter(|n| n.0 == device.0).map(|n| (n.1, n.2, n.3, n.4)).collect()
    }
}

// ───────────────────────────── Builders ─────────────────────────────

/// A normalised value for a stock device's parameter given in its own units.
fn norm(plugin: &str, param: u32, plain: f32) -> f32 {
    let p = aitrack_stock::create(plugin).expect("a stock device");
    p.info().params.iter().find(|i| i.id == param).expect("a parameter").to_normalized(plain)
}

struct B {
    p: Project,
    track: TrackId,
}

impl B {
    fn new() -> Self {
        let mut p = Project::new("stock midi");
        let track = p.alloc_track_id();
        p.tracks.push(Track::new(track, TrackKind::Instrument, "t", Color(0)));
        Self { p, track }
    }

    /// A stock device with parameters given in plain units, as `(id, value)`.
    fn device(&mut self, plugin: &str, params: &[(u32, f32)]) -> DeviceId {
        let d = self.make(plugin, params);
        let id = d.id;
        self.p.track_mut(self.track).unwrap().devices.push(d);
        id
    }

    fn make(&mut self, plugin: &str, params: &[(u32, f32)]) -> Device {
        let id = self.p.alloc_device_id();
        let mut d = Device::stock(id, plugin, plugin);
        for &(k, v) in params {
            d.params.insert(k, if plugin.starts_with("midi.") { norm(plugin, k, v) } else { v });
        }
        d
    }

    /// A clip of notes `(key, start tick, length ticks)`.
    fn notes(&mut self, notes: &[(u8, Tick, Tick)]) {
        let sid = self.p.alloc_source_id();
        let mut src = NoteSource::default();
        for &(key, start, len) in notes {
            let nid = self.p.alloc_note_id();
            src.notes.push(Note::new(nid, key, start, len, 1.0));
        }
        self.p.sources.insert(sid, Source::Notes(src));
        let cid = self.p.alloc_clip_id();
        self.p.clips.insert(cid, Clip::new("c", Color(0), sid, 6 * PPQ));
        let pid = self.p.alloc_placement_id();
        self.p.track_mut(self.track).unwrap().arrangement.push(Placement { id: pid, clip: cid, start: 0, lane: 0 });
    }

    fn render(&self, f: &Stock, block: usize, ticks: Tick) -> AudioBuffer {
        render_offline(&self.p, f, (0, ticks), SR, block).unwrap()
    }
}

fn start(p: &Project, f: Arc<Stock>) -> (EngineHandle, AudioProcessor) {
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

/// Keys still sounding at the end of a probe log (an all-notes-off clears all).
fn open_keys(notes: &[Seen]) -> Vec<u8> {
    let mut on: Vec<u8> = Vec::new();
    for (is_on, key, _, _) in notes {
        if *is_on {
            on.push(*key);
        } else if *key == 255 {
            on.clear();
        } else if let Some(i) = on.iter().position(|k| k == key) {
            on.remove(i);
        }
    }
    on
}

/// No key is started twice without ending in between.
fn never_doubled(notes: &[Seen]) -> bool {
    let mut on = [false; 128];
    for (is_on, key, _, _) in notes {
        if *key == 255 {
            on = [false; 128];
        } else if *is_on {
            if on[*key as usize] {
                return false;
            }
            on[*key as usize] = true;
        } else {
            on[*key as usize] = false;
        }
    }
    true
}

/// Arpeggiator set up for the tests: Up, 1/16, gate 50 %, locked to the grid.
fn arp_params() -> Vec<(u32, f32)> {
    vec![(arp::p::STYLE, 0.0), (arp::p::RATE, 7.0), (arp::p::GATE, 50.0), (arp::p::RETRIGGER, 0.0), (arp::p::OCTAVES, 1.0)]
}

/// Echo: 1/8, tempo-synced, two repeats at the same level.
fn echo_params() -> Vec<(u32, f32)> {
    vec![(echo::p::DIVISION, 6.0), (echo::p::SYNC, 1.0), (echo::p::REPEATS, 2.0), (echo::p::FEEDBACK, 100.0), (echo::p::LENGTH, echo::length::ORIGINAL), (echo::p::GATE, 100.0)]
}

const CHORD: [(u8, Tick, Tick); 3] = [(60, 0, 3 * PPQ), (64, 0, 3 * PPQ), (67, 0, 3 * PPQ)];

// ───────────────────────────── Where the notes land ─────────────────────────────

#[test]
fn the_arpeggiator_plays_its_steps_on_their_exact_samples() {
    let mut b = B::new();
    // The instrument first in the model, the arpeggiator after it: it still
    // runs before the instrument.
    b.device(TEST_SYNTH, &[(0, 1.0)]);
    b.device(arp::ID, &arp_params());
    let probe = b.device(KEYS, &[]);
    b.notes(&CHORD);
    let f = Stock::default();
    let out = b.render(&f, BLOCK, 3 * PPQ);
    let got = f.notes(probe);
    let step = BEAT / 4;
    let keys = [60u8, 64, 67];
    // 12 steps in 3 beats; each is a note-on on the step and its note-off at half a step.
    let want_on: Vec<(u8, i64)> = (0..12).map(|k| (keys[k as usize % 3], k * step)).collect();
    let ons: Vec<(u8, i64)> = got.iter().filter(|n| n.0).map(|n| (n.1, n.3)).collect();
    assert_eq!(ons[..12], want_on[..], "arpeggio: {got:?}");
    for (k, (key, at)) in want_on.iter().enumerate() {
        assert!(got.contains(&(false, *key, 0, at + step / 2)), "step {k}: note-off half a step later in {got:?}");
    }
    // And the instrument, although listed first in the model, heard it: one
    // onset per step.
    let l = out.channel(0);
    let onsets: Vec<usize> = (0..l.len()).filter(|&i| l[i].abs() > 1e-6 && (i == 0 || l[i - 1].abs() <= 1e-6)).collect();
    assert_eq!(onsets[..12], (0..12).map(|k| (k * step) as usize).collect::<Vec<_>>()[..], "{:?}", &onsets[..onsets.len().min(14)]);
}

#[test]
fn the_echo_repeats_on_the_grid() {
    let mut b = B::new();
    b.device(echo::ID, &echo_params());
    let probe = b.device(KEYS, &[]);
    b.notes(&[(60, 0, PPQ)]);
    let f = Stock::default();
    b.render(&f, BLOCK, 3 * PPQ);
    let got = f.notes(probe);
    let ons: Vec<i64> = got.iter().filter(|n| n.0).map(|n| n.3).collect();
    let offs: Vec<i64> = got.iter().filter(|n| !n.0).map(|n| n.3).collect();
    assert_eq!(ons, vec![0, BEAT / 2, BEAT], "the note and its two repeats, an eighth apart: {got:?}");
    // A key never carries two notes: each repeat cuts the one before it (the
    // echo's own ledger); the last runs as long as the played note.
    assert_eq!(offs, vec![BEAT / 2, BEAT, 2 * BEAT], "{got:?}");
    assert!(never_doubled(&got));
}

// ───────────────────────────── Live is offline ─────────────────────────────

fn arp_then_echo() -> (B, DeviceId) {
    let mut b = B::new();
    b.device(arp::ID, &arp_params());
    b.device(echo::ID, &echo_params());
    let probe = b.device(KEYS, &[]);
    b.notes(&CHORD);
    (b, probe)
}

#[test]
fn live_and_offline_give_the_same_notes_for_any_block_size() {
    let (b, probe) = arp_then_echo();
    let upto = 3 * BEAT;
    let notes = |f: &Stock| f.notes(probe).into_iter().filter(|n| n.3 < upto).collect::<Vec<_>>();
    let mut runs = Vec::new();
    for block in [128, 256, 1024] {
        let f = Stock::default();
        b.render(&f, block, 3 * PPQ);
        runs.push(notes(&f));
    }
    assert!(runs[0].len() > 20, "{} events", runs[0].len());
    assert_eq!(runs[0], runs[1]);
    assert_eq!(runs[0], runs[2]);
    // Live: the same transport, block by block.
    let f = Arc::new(Stock::default());
    let (h, mut proc_) = start(&b.p, f.clone());
    h.play();
    blocks(&mut proc_, (upto as usize) / BLOCK + 8);
    assert_eq!(notes(&f), runs[0], "live playback is the offline render");
    assert!(never_doubled(&f.notes(probe)));
}

// ───────────────────────────── Nothing is left open ─────────────────────────────

/// Hold a chord live, let the pattern run, do `act`, and see what is left.
fn after(act: impl FnOnce(&EngineHandle, &mut AudioProcessor, &mut B), setup: impl FnOnce(&mut B)) -> Vec<Seen> {
    let (mut b, probe) = {
        let mut b = B::new();
        b.device(arp::ID, &arp_params());
        b.device(echo::ID, &echo_params());
        let probe = b.device(KEYS, &[]);
        setup(&mut b);
        (b, probe)
    };
    let track = b.track;
    let f = Arc::new(Stock::default());
    let (h, mut proc_) = start(&b.p, f.clone());
    h.play();
    for key in [60, 64, 67] {
        h.send_midi_to(track, MidiMessage::note_on(0, key, 100));
    }
    // Long enough for the arpeggio and the echoes to be in full swing.
    blocks(&mut proc_, 60);
    assert!(f.notes(probe).iter().filter(|n| n.0).count() >= 3, "it is playing: {:?}", f.notes(probe));
    act(&h, &mut proc_, &mut b);
    blocks(&mut proc_, 120);
    f.notes(probe)
}

#[test]
fn stopping_leaves_no_note_open() {
    let got = after(|h, _, _| h.stop(), |_| {});
    assert!(open_keys(&got).is_empty(), "{got:?}");
    assert!(never_doubled(&got));
}

#[test]
fn jumping_leaves_no_note_open_and_the_pattern_goes_on() {
    let got = after(|h, _, _| h.seek(2 * PPQ), |_| {});
    // (The held keys were consumed by the arpeggiator at once: after the jump it
    // has nothing to play, the open ones are closed.)
    assert!(open_keys(&got).is_empty(), "{got:?}");
    assert!(never_doubled(&got));
}

#[test]
fn a_loop_wrap_closes_what_was_open() {
    // A loop of one beat: the pattern wraps every 24 000 samples.
    // With the loop on, at no time are two notes of a key open at once.
    let mut b = B::new();
    b.device(arp::ID, &arp_params());
    b.device(echo::ID, &echo_params());
    let probe = b.device(KEYS, &[]);
    b.notes(&CHORD);
    let f = Arc::new(Stock::default());
    let (h, mut proc_) = start(&b.p, f.clone());
    h.set_loop(Some((0, PPQ)));
    h.play();
    blocks(&mut proc_, 400);
    let looping = f.notes(probe);
    assert!(never_doubled(&looping), "{looping:?}");
    // Every pass starts clean: the first thing after each wrap is a close-out
    // of the previous pass (an all-notes-off or note-offs at the loop start).
    let wraps = looping.iter().filter(|n| n.3 == 0 && n.0).count();
    assert!(wraps >= 3, "the pattern restarted on every pass: {wraps}");
    h.stop();
    blocks(&mut proc_, 4);
    assert!(open_keys(&f.notes(probe)).is_empty(), "{:?}", f.notes(probe));
}

#[test]
fn switching_a_device_off_leaves_no_note_open() {
    let got = after(
        |_, _, _| {},
        |b| {
            // The arpeggiator is automated off a beat in.
            let arp_id = b.p.track(b.track).unwrap().devices[0].id;
            let mut lane = AutomationLane::new(AutomationTarget::DeviceEnabled { device: arp_id });
            lane.points = vec![AutomationPoint { tick: 0, value: 1.0, curve: 0.0 }, AutomationPoint { tick: PPQ, value: 1.0, curve: 0.0 }, AutomationPoint { tick: PPQ + 1, value: 0.0, curve: 0.0 }];
            b.p.track_mut(b.track).unwrap().automation.push(lane);
        },
    );
    assert!(open_keys(&got).is_empty(), "{got:?}");
    assert!(never_doubled(&got));
}

#[test]
fn recompiling_leaves_no_note_open() {
    let got = after(
        |h, _, b| {
            b.p.track_mut(b.track).unwrap().mute = true;
            h.update_project(&b.p);
            h.flush();
        },
        |_| {},
    );
    assert!(open_keys(&got).is_empty(), "{got:?}");
    assert!(never_doubled(&got));
}

// ───────────────────────────── Order ─────────────────────────────

/// Velocity of the repeats the probe got, for a played note of velocity 127.
fn repeat_velocities(first: &str) -> Vec<u8> {
    let mut b = B::new();
    let fixed = [(velocity::p::MODE, 2.0), (velocity::p::FIXED, 100.0)];
    let eco = [(echo::p::DIVISION, 6.0), (echo::p::SYNC, 1.0), (echo::p::REPEATS, 1.0), (echo::p::FEEDBACK, 50.0)];
    if first == "velocity" {
        b.device(velocity::ID, &fixed);
        b.device(echo::ID, &eco);
    } else {
        b.device(echo::ID, &eco);
        b.device(velocity::ID, &fixed);
    }
    let probe = b.device(KEYS, &[]);
    b.notes(&[(60, 0, PPQ / 4)]);
    let f = Stock::default();
    b.render(&f, BLOCK, PPQ);
    f.notes(probe).iter().filter(|n| n.0).map(|n| n.2).collect()
}

#[test]
fn two_effects_apply_in_the_models_order() {
    // Velocity first: the echo repeats the 100 it was given, at half.
    assert_eq!(repeat_velocities("velocity"), vec![100, 50]);
    // Echo first: the repeat is 127 × 50 %, and the velocity effect then makes everything 100.
    assert_eq!(repeat_velocities("echo"), vec![100, 100]);
}

// ───────────────────────────── In a rack chain ─────────────────────────────

#[test]
fn inside_a_rack_chain_they_behave_the_same() {
    // On the track.
    let (b, probe) = {
        let mut b = B::new();
        b.device(arp::ID, &arp_params());
        let probe = b.device(KEYS, &[]);
        b.notes(&CHORD);
        (b, probe)
    };
    let f = Stock::default();
    b.render(&f, BLOCK, 3 * PPQ);
    let on_track = f.notes(probe);

    // In a chain of an Instrument Rack.
    let mut r = B::new();
    let rack_id = r.p.alloc_device_id();
    let arp_dev = r.make(arp::ID, &arp_params());
    let probe_dev = r.make(KEYS, &[]);
    let rack_probe = probe_dev.id;
    let mut chain = Chain::new("a", Color(0));
    chain.devices.push(arp_dev);
    chain.devices.push(probe_dev);
    let mut rack = Rack::new(RackKind::Instrument);
    rack.chains.push(chain);
    r.p.track_mut(r.track).unwrap().devices.push(Device { kind: DeviceKind::Rack(rack), ..Device::stock(rack_id, "rack", "Rack") });
    r.notes(&CHORD);
    let g = Stock::default();
    r.render(&g, BLOCK, 3 * PPQ);
    assert!(!on_track.is_empty());
    assert_eq!(g.notes(rack_probe), on_track, "the same arpeggio inside a chain");
}

// ───────────────────────────── Allocation ─────────────────────────────

#[test]
fn a_track_of_the_real_midi_effects_never_allocates() {
    let mut b = B::new();
    for id in ["midi.arp", "midi.chord", "midi.scale", "midi.velocity", "midi.echo", "midi.random", "midi.length", "midi.pitch"] {
        b.device(id, &[]);
    }
    b.device(TEST_SYNTH, &[]);
    let track = b.track;
    let (h, mut proc_) = start(&b.p, Arc::new(Stock::default()));
    h.set_loop(Some((0, PPQ)));
    h.play();
    blocks(&mut proc_, 8);
    let mut buf = AudioBuffer::new(2, BLOCK);
    let ((), allocs) = count_allocs(|| {
        for i in 0..600u32 {
            let key = 48 + (i % 24) as u8;
            h.send_midi_to(track, MidiMessage::note_on(0, key, 90));
            if i % 3 == 0 {
                h.send_midi_to(track, MidiMessage::note_off(0, key));
            }
            proc_.process_planar(&mut buf);
            if i == 300 {
                h.stop();
            }
            if i == 320 {
                h.play();
            }
            if i == 400 {
                h.seek(PPQ / 2);
            }
        }
    });
    assert_eq!(allocs, 0, "eight real MIDI effects, a loop, a stop, a restart and a jump");
}
