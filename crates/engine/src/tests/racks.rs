//! Racks as the engine plays them: Receive / Play notes, choke groups, chain
//! gain ramps, zones with fades and Chain Select, and the Drum Rack's
//! returns with their latency compensation.

use crate::export::render_offline;
use crate::factory::ProcessorFactory;
use crate::fallback::{FallbackFactory, TEST_GAIN, TEST_LATENCY, TEST_SYNTH};
use crate::{AudioProcessor, Engine, EngineConfig, EngineHandle, MidiMessage};
use aitrack_dsp::{AudioBuffer, Category, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor};
use aitrack_model::*;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;
const BEAT: usize = 24_000;

// ───────────────────────────── Probes ─────────────────────────────

const KEYS: &str = "probe.keys";
const DC: &str = "probe.dc";

#[derive(Default)]
struct Log {
    /// `(is note-on, key, velocity)` each device received, by device id.
    notes: HashMap<u64, Vec<(bool, u8, f32)>>,
    /// Process calls by device id.
    calls: HashMap<u64, usize>,
}

/// `KEYS` records the notes it gets; `DC` writes its parameter 0 into every
/// sample (ignoring its input). Both count their calls.
struct Probe {
    info: PluginInfo,
    id: u64,
    dc: bool,
    value: f32,
    log: Arc<Mutex<Log>>,
}

impl Processor for Probe {
    fn info(&self) -> &PluginInfo {
        &self.info
    }
    fn prepare(&mut self, _: f64, _: usize) {}
    fn reset(&mut self) {}
    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        let mut log = self.log.lock().unwrap();
        *log.calls.entry(self.id).or_default() += 1;
        for e in ctx.events {
            match e.kind {
                EventKind::NoteOn { key, velocity, .. } => log.notes.entry(self.id).or_default().push((true, key, velocity)),
                EventKind::NoteOff { key, .. } => log.notes.entry(self.id).or_default().push((false, key, 0.0)),
                _ => {}
            }
        }
        if self.dc {
            let (l, r) = ctx.buffer.stereo_mut();
            l.fill(self.value);
            r.fill(self.value);
        }
    }
    fn set_param(&mut self, id: u32, v: f32) {
        if id == 0 {
            self.value = v;
        }
    }
    fn get_param(&self, id: u32) -> f32 {
        if id == 0 { self.value } else { 0.0 }
    }
}

#[derive(Default)]
struct Probes {
    log: Arc<Mutex<Log>>,
}

impl ProcessorFactory for Probes {
    fn create(&self, device: &Device, _: f64, _: usize) -> Option<Box<dyn Processor>> {
        let DeviceKind::Stock { plugin_id } = &device.kind else { return None };
        let (id, dc) = match plugin_id.as_str() {
            KEYS => (KEYS, false),
            DC => (DC, true),
            _ => return None,
        };
        let info = PluginInfo {
            id,
            name: "Probe",
            vendor: "AI-Track",
            category: Category::Effect,
            tags: &[],
            params: vec![ParamInfo::new(0, "Value", 0.0, 1.0, 0.0)],
            inputs: 2,
            outputs: 2,
            has_sidechain: false,
        };
        Some(Box::new(Probe { info, id: device.id.0, dc, value: 0.0, log: self.log.clone() }))
    }
}

impl Probes {
    fn notes(&self, device: DeviceId) -> Vec<(bool, u8, f32)> {
        self.log.lock().unwrap().notes.get(&device.0).cloned().unwrap_or_default()
    }
    fn calls(&self, device: DeviceId) -> usize {
        self.log.lock().unwrap().calls.get(&device.0).copied().unwrap_or(0)
    }
}

// ───────────────────────────── Builders ─────────────────────────────

struct B {
    p: Project,
    track: TrackId,
}

impl B {
    fn new(kind: TrackKind) -> Self {
        let mut p = Project::new("rack");
        let track = p.alloc_track_id();
        p.tracks.push(Track::new(track, kind, "t", Color(0)));
        Self { p, track }
    }

    fn device(&mut self, plugin: &str, params: &[(u32, f32)]) -> Device {
        let id = self.p.alloc_device_id();
        let mut d = Device::stock(id, plugin, plugin);
        for &(k, v) in params {
            d.params.insert(k, v);
        }
        d
    }

    fn chain(&mut self, name: &str, devices: Vec<Device>) -> Chain {
        let mut c = Chain::new(name, Color(0));
        c.devices = devices;
        c
    }

    /// Put `rack` on the track; returns the rack device's id.
    fn rack(&mut self, rack: Rack) -> DeviceId {
        let id = self.p.alloc_device_id();
        let t = self.p.track_mut(self.track).unwrap();
        t.devices.push(Device { kind: DeviceKind::Rack(rack), ..Device::stock(id, "rack", "Rack") });
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

    fn render(&self, f: &dyn ProcessorFactory, ticks: Tick) -> AudioBuffer {
        render_offline(&self.p, f, (0, ticks), SR, BLOCK).unwrap()
    }
}

fn peak(l: &[f32]) -> f32 {
    l.iter().fold(0.0, |m, s| m.max(s.abs()))
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

fn block(proc_: &mut AudioProcessor) -> AudioBuffer {
    let mut buf = AudioBuffer::new(2, BLOCK);
    proc_.process_planar(&mut buf);
    buf
}

fn sine_pad(b: &mut B) -> Device {
    b.device(TEST_SYNTH, &[(0, 0.0), (5, 0.5)])
}

// ───────────────────────────── 2.9 · Drum Rack ─────────────────────────────

#[test]
fn a_pad_gets_its_play_note_and_the_note_off_finds_its_note() {
    let mut b = B::new(TrackKind::Instrument);
    let (ka, kb) = (b.device(KEYS, &[]), b.device(KEYS, &[]));
    let (ida, idb) = (ka.id, kb.id);
    let mut rack = Rack::new(RackKind::Drum);
    let mut a = b.chain("a", vec![ka]);
    a.note = Some(36);
    a.play_note = Some(72);
    let mut c = b.chain("b", vec![kb]);
    c.note = Some(38); // (no Play note: middle C)
    rack.chains.push(a);
    rack.chains.push(c);
    let rack_id = b.rack(rack);
    let f = Arc::new(Probes::default());
    let (h, mut proc_) = start(&b.p, f.clone());
    let track = b.track;
    h.send_midi_to(track, MidiMessage::note_on(0, 36, 100));
    h.send_midi_to(track, MidiMessage::note_on(0, 38, 80));
    h.send_midi_to(track, MidiMessage::note_on(0, 40, 80)); // (no pad)
    block(&mut proc_);
    // The pad's Play note is edited while its note is held: the note-off still
    // goes to the key the note-on went to.
    let mut p2 = b.p.clone();
    let DeviceKind::Rack(r) = &mut p2.tracks[1].devices[0].kind else { unreachable!() };
    r.chains[0].play_note = Some(80);
    let _ = rack_id;
    h.update_project(&p2);
    h.flush();
    block(&mut proc_);
    h.send_midi_to(track, MidiMessage::note_off(0, 36));
    h.send_midi_to(track, MidiMessage::note_off(0, 38));
    block(&mut proc_);
    block(&mut proc_);
    let on = |key: u8, vel: f32| (true, key, vel);
    assert_eq!(f.notes(ida), vec![on(72, 100.0 / 127.0), (false, 72, 0.0)], "Receive 36 plays as 72, and stops as 72");
    assert_eq!(f.notes(idb), vec![on(60, 80.0 / 127.0), (false, 60, 0.0)], "no Play note: middle C");
}

fn choke_project(group_b: u8) -> (B, DeviceId) {
    let mut b = B::new(TrackKind::Instrument);
    let synth = sine_pad(&mut b);
    let silent = b.device(TEST_GAIN, &[]);
    let mut rack = Rack::new(RackKind::Drum);
    let mut open = b.chain("open", vec![synth]);
    open.note = Some(36);
    open.choke = 1;
    let mut closed = b.chain("closed", vec![silent]);
    closed.note = Some(38);
    closed.choke = group_b;
    rack.chains.push(open);
    rack.chains.push(closed);
    let id = b.rack(rack);
    // The open pad rings for two beats; the closed one is hit on the 2nd, at a
    // sample that is inside a block.
    b.notes(&[(36, 0, 2 * PPQ), (38, PPQ, 60)]);
    (b, id)
}

#[test]
fn a_pad_in_the_same_choke_group_cuts_the_other_within_5_ms() {
    let (b, _) = choke_project(1);
    let out = b.render(&FallbackFactory, 2 * PPQ);
    let l = out.channel(0);
    let hit = BEAT; // the closed pad's note-on
    let ring = peak(&l[hit - 2_000..hit - 1]);
    assert!(ring > 0.2, "the open pad rings before the hit: {ring}");
    // From the hit it fades out over 5 ms (240 samples) and stays silent
    // for the rest of the note, whatever the synth does (all notes off).
    let mid = peak(&l[hit + 100..hit + 140]);
    assert!(mid < ring * 0.75 && mid > 0.0, "halfway down: {mid} vs {ring}");
    assert!(peak(&l[hit + 245..hit + 4_000]) < 1e-4, "silent 5 ms after the hit: {}", peak(&l[hit + 245..hit + 4_000]));
    assert!(peak(&l[hit + 4_000..2 * BEAT - 100]) < 1e-4, "and it stays cut");
    // Without a shared group nothing is cut.
    let (b, _) = choke_project(0);
    let l = b.render(&FallbackFactory, 2 * PPQ).channel(0).to_vec();
    assert!(peak(&l[hit + 245..hit + 4_000]) > 0.2, "no choke group, no cut");
}

#[test]
fn a_pad_does_not_choke_itself_and_comes_back_when_hit_again() {
    let mut b = B::new(TrackKind::Instrument);
    let synth = sine_pad(&mut b);
    let mut rack = Rack::new(RackKind::Drum);
    let mut pad = b.chain("pad", vec![synth]);
    pad.note = Some(36);
    pad.choke = 1;
    rack.chains.push(pad);
    let silent = b.device(TEST_GAIN, &[]);
    let mut other = b.chain("other", vec![silent]);
    other.note = Some(38);
    other.choke = 1;
    rack.chains.push(other);
    b.rack(rack);
    // Pad 36, then 38 chokes it, then 36 again.
    b.notes(&[(36, 0, PPQ / 2), (38, PPQ / 2, 30), (36, PPQ, PPQ / 2)]);
    let l = b.render(&FallbackFactory, 2 * PPQ).channel(0).to_vec();
    assert!(peak(&l[BEAT / 2 + 245..BEAT - 100]) < 1e-4, "choked");
    assert!(peak(&l[BEAT + 2_000..BEAT + 4_000]) > 0.2, "hit again: it plays");
}

#[test]
fn changing_a_chain_volume_does_not_step() {
    let mut b = B::new(TrackKind::Instrument);
    let synth = sine_pad(&mut b);
    let mut rack = Rack::new(RackKind::Drum);
    let mut pad = b.chain("pad", vec![synth]);
    pad.note = Some(36);
    rack.chains.push(pad);
    b.rack(rack);
    let track = b.track;
    let (h, mut proc_) = start(&b.p, Arc::new(FallbackFactory));
    h.send_midi_to(track, MidiMessage::note_on(0, 36, 127));
    let mut last = Vec::new();
    for _ in 0..6 {
        last = block(&mut proc_).channel(0).to_vec();
    }
    let steady = last.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max);
    let before = peak(&last);
    // -20 dB on the pad while it rings: a recompile and a new graph.
    let mut p2 = b.p.clone();
    let DeviceKind::Rack(r) = &mut p2.tracks[1].devices[0].kind else { unreachable!() };
    r.chains[0].volume_db = -20.0;
    h.update_project(&p2);
    h.flush();
    // (From the last sample before the swap: the step would be there.)
    let mut swap = vec![*last.last().unwrap()];
    swap.extend_from_slice(block(&mut proc_).channel(0));
    let step = swap.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max);
    assert!(step < steady * 1.3, "no step at the swap: {step} vs {steady} in steady state");
    let after = peak(block(&mut proc_).channel(0));
    assert!((after / before - 0.1).abs() < 0.02, "settles at -20 dB: {after} vs {before}");
}

// ───────────────────────────── 2.10 · Zones with fades, Chain Select ─────────────────────────────

fn select_project(selector: u8) -> (B, DeviceId, DeviceId, DeviceId) {
    let mut b = B::new(TrackKind::Audio);
    let (da, db) = (b.device(DC, &[(0, 0.4)]), b.device(DC, &[(0, 0.8)]));
    let (ida, idb) = (da.id, db.id);
    let mut rack = Rack::new(RackKind::Audio);
    // Two zones that cross at 64: A fades out 48 → 80, B fades in 48 → 80.
    let mut a = b.chain("a", vec![da]);
    a.selector_range = (0, 48);
    a.sel_fade = (0, 32);
    let mut c = b.chain("b", vec![db]);
    c.selector_range = (80, 127);
    c.sel_fade = (32, 0);
    rack.chains.push(a);
    rack.chains.push(c);
    rack.chain_selector = selector;
    let rack_id = b.rack(rack);
    (b, rack_id, ida, idb)
}

#[test]
fn chain_select_crossfades_and_a_chain_at_zero_sleeps() {
    // At 64 each is half way: 0.5 × 0.4 + 0.5 × 0.8.
    let (b, _, ida, _) = select_project(64);
    let f = Probes::default();
    let l = b.render(&f, PPQ).channel(0).to_vec();
    assert!((l[5_000] - 0.6).abs() < 1e-3, "{}", l[5_000]);
    assert!(f.calls(ida) > 0);

    // At 127 only B is in its zone; A fades out and then sleeps.
    let (mut b, rack_id, ida, idb) = select_project(64);
    let f = Arc::new(Probes::default());
    let (h, mut proc_) = start(&b.p, f.clone());
    for _ in 0..3 {
        block(&mut proc_);
    }
    let DeviceKind::Rack(r) = &mut b.p.tracks[1].devices[0].kind else { unreachable!() };
    r.chain_selector = 127;
    let _ = rack_id;
    h.update_project(&b.p);
    h.flush();
    let ramp = block(&mut proc_);
    assert!(ramp.channel(0)[0] < 0.61 && ramp.channel(0)[0] > 0.7 * 0.8 - 0.1, "gliding, not stepping: {}", ramp.channel(0)[0]);
    let settled = block(&mut proc_);
    assert!((settled.channel(0)[10] - 0.8).abs() < 1e-3, "{}", settled.channel(0)[10]);
    let (calls_a, calls_b) = (f.calls(ida), f.calls(idb));
    for _ in 0..10 {
        block(&mut proc_);
    }
    assert_eq!(f.calls(ida), calls_a, "the chain at gain 0 is not processed");
    assert!(f.calls(idb) > calls_b, "the other one is");
    // Back in its zone it wakes up.
    let DeviceKind::Rack(r) = &mut b.p.tracks[1].devices[0].kind else { unreachable!() };
    r.chain_selector = 0;
    h.update_project(&b.p);
    h.flush();
    for _ in 0..3 {
        block(&mut proc_);
    }
    assert!(f.calls(ida) > calls_a, "awake again");
    assert!((block(&mut proc_).channel(0)[10] - 0.4).abs() < 1e-3);
}

#[test]
fn key_and_velocity_fades_attenuate_the_note() {
    let mut b = B::new(TrackKind::Instrument);
    let k = b.device(KEYS, &[]);
    let id = k.id;
    let mut rack = Rack::new(RackKind::Instrument);
    let mut c = b.chain("zone", vec![k]);
    c.key_range = (60, 72);
    c.key_fade = (4, 12);
    c.velocity_range = (20, 100);
    c.vel_fade = (0, 27);
    rack.chains.push(c);
    b.rack(rack);
    let track = b.track;
    let f = Arc::new(Probes::default());
    let (h, mut proc_) = start(&b.p, f.clone());
    // Inside both zones: untouched. 78 is half way down the high key fade
    // (72 + 12 − 78 = 6 of 12) and 58 half way up the low one (58 − 56 = 2 of
    // 4). 90 is out. The velocity 114 is 13 of the 27 down its fade
    // (100 + 27 − 114).
    for (key, vel) in [(66u8, 64u8), (78, 100), (58, 100), (90, 100), (66, 114)] {
        h.send_midi_to(track, MidiMessage::note_on(0, key, vel));
    }
    block(&mut proc_);
    let mut got: Vec<(u8, f32)> = f.notes(id).into_iter().map(|n| (n.1, n.2)).collect();
    let mut want: [(u8, f32); 4] = [(66, 64.0 / 127.0), (78, 100.0 / 127.0 * 0.5), (58, 100.0 / 127.0 * 0.5), (66, 114.0 / 127.0 * 13.0 / 27.0)];
    // (Notes sent together reach the chain in no fixed order.)
    got.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
    want.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
    assert_eq!(got.len(), want.len(), "{got:?}");
    for ((gk, gv), (wk, wv)) in got.iter().zip(want) {
        assert_eq!(*gk, wk);
        assert!((gv - wv).abs() < 1e-4, "key {gk}: velocity {gv} vs {wv}");
    }
}

// ───────────────────────────── 2.11 · Returns and sends ─────────────────────────────

fn kit_project(send_db: f32, output: ChainOut) -> B {
    let mut b = B::new(TrackKind::Audio);
    let pad_dev = b.device(DC, &[(0, 0.5)]);
    // The return halves what it gets: gain 0.25 × 2.
    let fx = b.device(TEST_GAIN, &[(0, 0.25)]);
    let mut rack = Rack::new(RackKind::Drum);
    let mut pad = b.chain("pad", vec![pad_dev]);
    pad.note = Some(36);
    pad.sends = vec![send_db];
    pad.output = output;
    rack.chains.push(pad);
    let reverb = b.chain("verb", vec![fx]);
    rack.returns.push(reverb);
    b.rack(rack);
    b
}

#[test]
fn a_pad_sends_into_the_kits_return() {
    let f = Probes::default();
    // Dry 0.5 plus a send at 0 dB through a ×0.5 return.
    let l = kit_project(0.0, ChainOut::Rack).render(&f, PPQ).channel(0).to_vec();
    assert!((l[5_000] - 0.75).abs() < 1e-3, "{}", l[5_000]);
    // Send off: just the pad.
    let l = kit_project(SEND_OFF_DB, ChainOut::Rack).render(&f, PPQ).channel(0).to_vec();
    assert!((l[5_000] - 0.5).abs() < 1e-3, "{}", l[5_000]);
    // Straight into the return: only the wet path, at the send level (pad
    // output is not also in the rack).
    let l = kit_project(0.0, ChainOut::Return(0)).render(&f, PPQ).channel(0).to_vec();
    assert!((l[5_000] - 0.25).abs() < 1e-3, "{}", l[5_000]);
}

#[test]
fn a_returns_latency_is_compensated_like_a_chains() {
    let mut b = B::new(TrackKind::Instrument);
    let synth = b.device(TEST_SYNTH, &[(0, 1.0)]);
    let mut lat = b.device(TEST_LATENCY, &[]);
    lat.state = Some(300u32.to_le_bytes().to_vec());
    let mut rack = Rack::new(RackKind::Drum);
    let mut pad = b.chain("pad", vec![synth]);
    pad.note = Some(60);
    pad.sends = vec![0.0];
    rack.chains.push(pad);
    rack.returns.push(b.chain("delay", vec![lat]));
    b.rack(rack);
    b.notes(&[(60, PPQ, PPQ)]);
    let g = crate::compile::compile(&b.p, &mut crate::compile::CompileContext {
        factory: &FallbackFactory,
        pool: &mut crate::shared::ProcessorPool::new(),
        samples: &crate::sample_cache::SampleCache::blocking(),
        sample_rate: SR,
        max_frames: BLOCK,
        prev_meters: None,
        taps: Vec::new(),
        analysis: None,
        streams: None,
    })
    .unwrap();
    assert_eq!(g.total_latency(), 300, "the rack reports its slowest return");
    let l = b.render(&FallbackFactory, 2 * PPQ).channel(0).to_vec();
    // Dry and wet hit together, once, at the note.
    let onsets: Vec<usize> = l.iter().enumerate().filter(|(i, s)| s.abs() > 1e-6 && (*i == 0 || l[i - 1].abs() <= 1e-6)).map(|(i, _)| i).collect();
    assert_eq!(onsets, vec![BEAT], "{onsets:?}");
}

#[test]
fn moving_chain_select_never_compiles() {
    let (mut b, rack_id, ida, _) = select_project(64);
    let f = Arc::new(Probes::default());
    let (h, mut proc_) = start(&b.p, f.clone());
    block(&mut proc_);
    let compiles = h.compile_count();
    // The app's drag: the direct call, then the project.
    for v in (64..=127u8).step_by(9) {
        assert!(h.set_rack_selector(rack_id, v));
        let DeviceKind::Rack(r) = &mut b.p.tracks[1].devices[0].kind else { unreachable!() };
        r.chain_selector = v;
        h.update_project(&b.p);
        h.flush();
        block(&mut proc_);
    }
    assert_eq!(h.compile_count(), compiles, "0 compiles while the selector moves");
    for _ in 0..3 {
        block(&mut proc_);
    }
    // 127: only B is left, and A has gone to sleep.
    assert!((block(&mut proc_).channel(0)[10] - 0.8).abs() < 1e-3);
    let calls = f.calls(ida);
    block(&mut proc_);
    assert_eq!(f.calls(ida), calls);
}

#[test]
fn a_chain_waking_from_sleep_plays_nothing_from_before() {
    // Chain A (a DC source) shares the rack with chain B (300 ms of latency),
    // so A sits behind a delay line of 3 584 samples that is full of A's DC
    // while it plays.
    let mut b = B::new(TrackKind::Audio);
    let (da, lat) = (b.device(DC, &[(0, 0.4)]), {
        let mut l = b.device(TEST_LATENCY, &[]);
        l.state = Some(3_584u32.to_le_bytes().to_vec());
        l
    });
    let ida = da.id;
    let mut rack = Rack::new(RackKind::Audio);
    let mut a = b.chain("a", vec![da]);
    a.selector_range = (0, 48);
    let mut c = b.chain("b", vec![lat]);
    c.selector_range = (0, 127);
    rack.chains.push(a);
    rack.chains.push(c);
    let rack_id = b.rack(rack);
    let (h, mut proc_) = start(&b.p, Arc::new(Probes::default()));
    for _ in 0..30 {
        block(&mut proc_);
    }
    assert!((block(&mut proc_).channel(0)[10] - 0.4).abs() < 1e-3, "A plays");
    // Out of A's zone: it fades out and sleeps, for a long time.
    assert!(h.set_rack_selector(rack_id, 127));
    for _ in 0..200 {
        block(&mut proc_);
    }
    // While it sleeps its source changes to silence...
    let mut p2 = b.p.clone();
    p2.tracks[1].devices[0].kind = {
        let DeviceKind::Rack(r) = &p2.tracks[1].devices[0].kind else { unreachable!() };
        let mut r = r.clone();
        r.chains[0].devices[0].params.insert(0, 0.0);
        r.chain_selector = 127;
        DeviceKind::Rack(r)
    };
    h.update_project(&p2);
    h.flush();
    block(&mut proc_);
    let _ = ida;
    // ...and A comes back: the old 0.4 in its delay line must not be heard.
    assert!(h.set_rack_selector(rack_id, 0));
    let mut heard = 0.0f32;
    for _ in 0..24 {
        heard = heard.max(peak(block(&mut proc_).channel(0)));
    }
    assert!(heard < 1e-6, "audio from before the sleep came out: peak {heard}");
}
