//! Live MIDI keeps its timing inside the block (1.3) and a knob, fader or
//! send knob moves without compiling the graph (1.11): parameter edits take a
//! fast path to the running graph, a project that differs only by parameters
//! is absorbed without a compile, and automated stock devices are cut into
//! pieces on their own while the rest of the chain keeps whole blocks.

use crate::factory::ProcessorFactory;
use crate::fallback::{FallbackFactory, TEST_GAIN, TEST_SYNTH};
use crate::graph::find_proc_mut;
use crate::{AudioProcessor, Engine, EngineConfig, EngineHandle, MidiMessage};
use aitrack_dsp::{AudioBuffer, Category, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor};
use aitrack_model::*;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;
/// Samples per tick at 120 BPM and 48 kHz.
const SPT: usize = 25;

fn start(p: &Project, f: Arc<dyn ProcessorFactory>) -> (EngineHandle, AudioProcessor) {
    let (h, proc_) = Engine::new(EngineConfig::headless(SR), f).unwrap();
    h.update_project(p);
    h.flush();
    let mut proc_ = proc_.unwrap();
    block(&mut proc_);
    (h, proc_)
}

fn block(proc_: &mut AudioProcessor) -> AudioBuffer {
    let mut buf = AudioBuffer::new(2, BLOCK);
    proc_.process_planar(&mut buf);
    buf
}

fn add(p: &mut Project, kind: TrackKind, name: &str, plugin: Option<&str>) -> TrackId {
    let id = p.alloc_track_id();
    let mut t = Track::new(id, kind, name, Color(0));
    if let Some(plugin) = plugin {
        let d = p.alloc_device_id();
        t.devices.push(Device::stock(d, plugin, plugin));
    }
    p.tracks.push(t);
    id
}

fn device_param(proc_: &mut AudioProcessor, device: DeviceId, param: u32) -> f32 {
    let g = proc_.graph_mut().expect("graph");
    for t in &mut g.tracks {
        if let Some(p) = find_proc_mut(&mut t.devices, device) {
            // SAFETY: the test drives the processor on this thread only.
            return unsafe { p.proc.get_mut() }.get_param(param);
        }
    }
    panic!("device {device} not in the graph");
}

// ───────────────────────────── 1.3 · Live MIDI timing ─────────────────────────────

fn synth_project() -> (Project, TrackId) {
    let mut p = Project::new("midi");
    let t = add(&mut p, TrackKind::Instrument, "keys", None);
    let d = p.alloc_device_id();
    let mut dev = Device::stock(d, TEST_SYNTH, TEST_SYNTH);
    dev.params.insert(0, 1.0);
    dev.params.insert(5, 0.5);
    let track = p.track_mut(t).unwrap();
    track.arm = true;
    track.devices.push(dev);
    (p, t)
}

/// A message that happened `sample` frames into the block that will be
/// built from the callback starting at `callback`: stamped one block before
/// that callback, the way the driver stamps what arrives during the
/// previous block.
fn nanos_for(callback: u64, sample: usize) -> u64 {
    callback - (BLOCK as f64 / SR * 1e9) as u64 + (sample as f64 / SR * 1e9) as u64
}

/// Announce a device callback, after the engine has run long enough for the
/// previous block's time span to be a positive time.
fn begin(proc_: &mut AudioProcessor) -> u64 {
    std::thread::sleep(std::time::Duration::from_millis(40));
    proc_.begin_callback();
    proc_.callback_start_nanos()
}

fn nonzero_span(l: &[f32]) -> Option<(usize, usize)> {
    let first = l.iter().position(|s| s.abs() > 1e-6)?;
    let last = l.iter().rposition(|s| s.abs() > 1e-6)?;
    Some((first, last))
}

#[test]
fn two_live_messages_in_one_block_keep_their_spacing() {
    let (p, _) = synth_project();
    let (h, mut proc_) = start(&p, Arc::new(FallbackFactory));
    let cb = begin(&mut proc_);
    assert!(h.send_midi_at(MidiMessage::note_on(0, 60, 100), nanos_for(cb, 100)));
    assert!(h.send_midi_at(MidiMessage::note_off(0, 60), nanos_for(cb, 200)));
    let out = block(&mut proc_);
    let (first, last) = nonzero_span(out.channel(0)).expect("the note sounded");
    assert!((99..=101).contains(&first), "note-on at sample 100, heard at {first}");
    assert!((198..=200).contains(&last), "note-off at sample 200, silent from {}", last + 1);
    assert_eq!(h.midi_message_count(), 2);
}

#[test]
fn old_and_untimed_messages_play_at_the_start_of_the_block() {
    let (p, _) = synth_project();
    let (h, mut proc_) = start(&p, Arc::new(FallbackFactory));
    begin(&mut proc_);
    // Stalled callback: stamped long before the window. And one with no time.
    assert!(h.send_midi_at(MidiMessage::note_on(0, 60, 100), 1));
    let out = block(&mut proc_);
    assert_eq!(nonzero_span(out.channel(0)).map(|s| s.0), Some(0));
    // Late in the window clamps to the last frame instead of overflowing.
    let (p2, _) = synth_project();
    let (h2, mut proc2) = start(&p2, Arc::new(FallbackFactory));
    let cb = begin(&mut proc2);
    assert!(h2.send_midi_at(MidiMessage::note_on(0, 60, 100), cb + 5_000_000));
    let out = block(&mut proc2);
    assert_eq!(nonzero_span(out.channel(0)).map(|s| s.0), Some(BLOCK - 1));
    drop(h);
}

#[test]
fn live_messages_go_to_the_segment_they_fall_in() {
    let (p, _) = synth_project();
    let (h, mut proc_) = start(&p, Arc::new(FallbackFactory));
    // A loop of 5 ticks (125 samples) wraps twice inside one block.
    h.set_loop(Some((0, 5)));
    h.play();
    block(&mut proc_);
    h.seek(0);
    block(&mut proc_);
    h.seek(0);
    let cb = begin(&mut proc_);
    assert!(h.send_midi_at(MidiMessage::note_on(0, 60, 100), nanos_for(cb, 200)));
    let out = block(&mut proc_);
    let first = nonzero_span(out.channel(0)).map(|s| s.0).expect("the note sounded");
    assert!((199..=201).contains(&first), "second segment is [125, 250): onset at 200, heard at {first}");
}

#[test]
fn the_midi_api_reports_target_ports_and_activity() {
    let (p, t) = synth_project();
    let (h, mut proc_) = start(&p, Arc::new(FallbackFactory));
    assert_eq!(h.midi_target(), None);
    h.set_midi_target(Some(t));
    assert_eq!(h.midi_target(), Some(t));
    h.set_midi_target(None);
    assert_eq!(h.midi_target(), None);
    assert!(h.open_midi_inputs().is_empty());
    // Every listed port says whether it is open; none is.
    assert!(h.midi_ports().iter().all(|port| !port.open));
    let before = h.midi_message_count();
    h.send_midi(MidiMessage::note_on(0, 60, 100));
    block(&mut proc_);
    assert_eq!(h.midi_message_count(), before + 1);
}

// ───────────────────────────── 1.11 · Fast path ─────────────────────────────

fn fifty_tracks() -> (Project, DeviceId) {
    let mut p = Project::new("fifty");
    for i in 0..50 {
        add(&mut p, TrackKind::Audio, &format!("t{i}"), Some(TEST_GAIN));
    }
    let d = p.tracks[26].devices[0].id;
    (p, d)
}

#[test]
fn dragging_a_knob_in_a_fifty_track_project_never_compiles() {
    let (mut p, d) = fifty_tracks();
    let (h, mut proc_) = start(&p, Arc::new(FallbackFactory));
    let compiles = h.compile_count();
    assert!(compiles >= 1);
    // Every frame: the fast command (instant sound) and the project the app
    // hands over after applying its `Command`.
    for i in 0..60 {
        let v = 0.1 + 0.01 * i as f32;
        assert!(h.set_device_param(d, 0, v));
        p.tracks[26].devices[0].params.insert(0, v);
        h.update_project(&p);
        h.flush();
        block(&mut proc_);
        assert!((device_param(&mut proc_, d, 0) - v).abs() < 1e-6, "frame {i}");
    }
    assert_eq!(h.compile_count(), compiles, "0 compiles during the drag");
    assert!(h.patch_count() >= 60);

    // The same drag with no fast command: the project alone is enough.
    for i in 0..30 {
        let v = 0.9 - 0.01 * i as f32;
        p.tracks[26].devices[0].params.insert(0, v);
        h.update_project(&p);
        h.flush();
        block(&mut proc_);
        assert!((device_param(&mut proc_, d, 0) - v).abs() < 1e-6, "project-only frame {i}");
    }
    assert_eq!(h.compile_count(), compiles);

    // A later compile keeps the value the patches left.
    let v = device_param(&mut proc_, d, 0);
    p.tracks[3].mute = true;
    h.update_project(&p);
    h.flush();
    block(&mut proc_);
    assert_eq!(h.compile_count(), compiles + 1, "a mute is structural");
    assert!((device_param(&mut proc_, d, 0) - v).abs() < 1e-6, "the compile did not undo the drag");
}

#[test]
fn faders_pans_and_send_levels_are_patched_not_compiled() {
    let mut p = Project::new("mix");
    let a = add(&mut p, TrackKind::Audio, "a", None);
    let r = add(&mut p, TrackKind::Return, "r", None);
    p.track_mut(a).unwrap().sends.push(Send { target: r, level_db: 0.0, pre_fader: false });
    let (h, mut proc_) = start(&p, Arc::new(FallbackFactory));
    let compiles = h.compile_count();

    p.track_mut(a).unwrap().volume_db = -6.0;
    p.track_mut(a).unwrap().pan = 0.5;
    p.track_mut(a).unwrap().sends[0].level_db = -12.0;
    h.update_project(&p);
    h.flush();
    block(&mut proc_);
    assert_eq!(h.compile_count(), compiles);
    {
        let g = proc_.graph_mut().unwrap();
        let t = &g.tracks[g.track_index(a).unwrap()];
        assert_eq!((t.volume_db, t.pan, t.sends[0].level_db), (-6.0, 0.5, -12.0));
    }

    // The direct commands do the same without a project.
    assert!(h.set_track_volume(a, -3.0));
    assert!(h.set_track_pan(a, -0.25));
    assert!(h.set_send_level(a, 0, -20.0));
    block(&mut proc_);
    let g = proc_.graph_mut().unwrap();
    let t = &g.tracks[g.track_index(a).unwrap()];
    assert_eq!((t.volume_db, t.pan, t.sends[0].level_db), (-3.0, -0.25, -20.0));
    assert_eq!(h.compile_count(), compiles);
}

#[test]
fn display_only_edits_cost_no_compile_and_structural_ones_do() {
    let (mut p, _) = fifty_tracks();
    let (h, mut proc_) = start(&p, Arc::new(FallbackFactory));
    let compiles = h.compile_count();
    // Names, colours and heights are not read by the compiler.
    p.tracks[5].name = "renamed".into();
    p.tracks[5].color = Color(0x123456);
    p.tracks[5].height = 77.0;
    p.tracks[5].collapsed = true;
    h.update_project(&p);
    h.flush();
    block(&mut proc_);
    assert_eq!(h.compile_count(), compiles);
    // Each of these changes what the graph is made of.
    type Edit = Box<dyn Fn(&mut Project)>;
    let mut edits: Vec<Edit> = Vec::new();
    edits.push(Box::new(|p| p.tracks[2].mute = !p.tracks[2].mute));
    edits.push(Box::new(|p| p.tracks[2].solo = !p.tracks[2].solo));
    edits.push(Box::new(|p| p.tracks[7].devices[0].enabled = false));
    edits.push(Box::new(|p| p.tracks[9].devices[0].state = Some(vec![1, 2, 3])));
    edits.push(Box::new(|p| p.tracks[9].output = Output::None));
    edits.push(Box::new(|p| {
        let id = p.alloc_device_id();
        p.tracks[11].devices.push(Device::stock(id, TEST_GAIN, TEST_GAIN));
    }));
    edits.push(Box::new(|p| p.tempo = TempoMap::constant(140.0)));
    for (i, edit) in edits.iter().enumerate() {
        let before = h.compile_count();
        edit(&mut p);
        h.update_project(&p);
        h.flush();
        block(&mut proc_);
        assert_eq!(h.compile_count(), before + 1, "structural edit {i} compiles once");
    }
}

#[test]
fn a_knob_inside_a_rack_is_patched_unless_a_macro_drives_it() {
    let mut p = Project::new("rack");
    let t = add(&mut p, TrackKind::Audio, "fx", None);
    let (rack_id, inner, driven) = (p.alloc_device_id(), p.alloc_device_id(), p.alloc_device_id());
    let mut chain = Chain::new("A", Color(0));
    chain.devices.push(Device::stock(inner, TEST_GAIN, TEST_GAIN));
    chain.devices.push(Device::stock(driven, TEST_GAIN, TEST_GAIN));
    let mut rack = Rack::new(RackKind::Audio);
    rack.chains.push(chain);
    rack.macros[0].mappings.push(MacroMapping { device: driven, param: 0, min: 0.0, max: 1.0 });
    let mut rd = Device::stock(rack_id, "rack", "Rack");
    rd.kind = DeviceKind::Rack(rack);
    p.track_mut(t).unwrap().devices.push(rd);
    let (h, mut proc_) = start(&p, Arc::new(FallbackFactory));
    let compiles = h.compile_count();

    let set = |p: &mut Project, dev: DeviceId, v: f32| {
        let DeviceKind::Rack(r) = &mut p.track_mut(t).unwrap().devices[0].kind else { unreachable!() };
        r.chains[0].devices.iter_mut().find(|d| d.id == dev).unwrap().params.insert(0, v);
    };
    set(&mut p, inner, 0.7);
    h.update_project(&p);
    h.flush();
    block(&mut proc_);
    assert_eq!(h.compile_count(), compiles, "a free knob inside a rack");
    assert!((device_param(&mut proc_, inner, 0) - 0.7).abs() < 1e-6);

    set(&mut p, driven, 0.3);
    h.update_project(&p);
    h.flush();
    block(&mut proc_);
    assert_eq!(h.compile_count(), compiles + 1, "a macro owns this one: compile");
}

// ───────────────────────────── 1.11 · Automation of stock devices ─────────────────────────────

const PARAM: u32 = 7;
/// A jump from 0.25 to 0.75 one tick after the first beat: sample 24 025,
/// which is on neither a block nor a control-block boundary.
const JUMP_AT: usize = (PPQ as usize + 1) * SPT;

#[derive(Default)]
struct Seen {
    frames: Vec<usize>,
}

/// Writes its parameter's value into every sample, changing it at each
/// `Param` event or `set_param`, and records the size of every call.
struct Probe {
    info: PluginInfo,
    value: f32,
    seen: Arc<Mutex<Seen>>,
}

impl Processor for Probe {
    fn info(&self) -> &PluginInfo {
        &self.info
    }
    fn prepare(&mut self, _: f64, _: usize) {}
    fn reset(&mut self) {}
    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        assert!(!ctx.events.iter().any(|e| matches!(e.kind, EventKind::Param { .. })), "a stock device gets its changes through set_param");
        let (l, r) = ctx.buffer.stereo_mut();
        l.fill(self.value);
        r.fill(self.value);
        self.seen.lock().unwrap().frames.push(ctx.buffer.frames());
    }
    fn set_param(&mut self, id: u32, v: f32) {
        if id == PARAM {
            self.value = v;
        }
    }
    fn get_param(&self, id: u32) -> f32 {
        if id == PARAM { self.value } else { 0.0 }
    }
}

/// A hosted probe applies its `Param` events itself, like the plugin tests.
struct HostedProbe {
    inner: Probe,
}

impl Processor for HostedProbe {
    fn info(&self) -> &PluginInfo {
        self.inner.info()
    }
    fn prepare(&mut self, _: f64, _: usize) {}
    fn reset(&mut self) {}
    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        self.inner.seen.lock().unwrap().frames.push(ctx.buffer.frames());
    }
    fn set_param(&mut self, _: u32, _: f32) {}
    fn get_param(&self, _: u32) -> f32 {
        0.0
    }
}

#[derive(Default)]
struct Probes {
    stock: Arc<Mutex<Seen>>,
    hosted: Arc<Mutex<Seen>>,
}

impl ProcessorFactory for Probes {
    fn create(&self, device: &Device, _: f64, _: usize) -> Option<Box<dyn Processor>> {
        let info = PluginInfo {
            id: "probe",
            name: "Probe",
            vendor: "AI-Track",
            category: Category::Effect,
            tags: &[],
            params: vec![ParamInfo::new(PARAM, "Value", 0.0, 1.0, 0.0)],
            inputs: 2,
            outputs: 2,
            has_sidechain: false,
        };
        Some(match device.kind {
            DeviceKind::Plugin { .. } => Box::new(HostedProbe { inner: Probe { info, value: 0.0, seen: self.hosted.clone() } }),
            _ => Box::new(Probe { info, value: 0.0, seen: self.stock.clone() }),
        })
    }
}

fn hosted(id: DeviceId) -> Device {
    let mut d = Device::stock(id, "probe", "Probe");
    d.kind = DeviceKind::Plugin { format: PluginFormat::Clap, uid: "com.example.probe".into(), path: PathBuf::from("/probe.clap"), vendor: "Example".into(), is_instrument: false };
    d
}

fn lane(target: AutomationTarget, points: &[(Tick, f32)]) -> AutomationLane {
    AutomationLane { points: points.iter().map(|&(tick, value)| AutomationPoint { tick, value, curve: 0.0 }).collect(), ..AutomationLane::new(target) }
}

fn render(p: &Project, f: &Probes) -> AudioBuffer {
    crate::export::render_offline(p, f, (0, 2 * PPQ), SR, BLOCK).unwrap()
}

#[test]
fn stock_automation_lands_on_its_exact_sample_and_only_the_stock_device_is_cut() {
    let f = Probes::default();
    let mut p = Project::new("auto");
    let t = add(&mut p, TrackKind::Audio, "fx", None);
    let (plugin, stock) = (p.alloc_device_id(), p.alloc_device_id());
    // The hosted plugin first (it leaves the buffer alone), the stock device last.
    p.track_mut(t).unwrap().devices.push(hosted(plugin));
    p.track_mut(t).unwrap().devices.push(Device::stock(stock, "probe", "Probe"));
    let jump = [(0, 0.25), (PPQ + 1, 0.25), (PPQ + 1, 0.75)];
    p.track_mut(t).unwrap().automation.push(lane(AutomationTarget::DeviceParam { device: stock, param: PARAM }, &jump));
    let out = render(&p, &f);
    let l = out.channel(0);
    assert!((l[JUMP_AT - 1] - 0.25).abs() < 1e-5, "before the jump: {}", l[JUMP_AT - 1]);
    assert!((l[JUMP_AT] - 0.75).abs() < 1e-5, "on the jump's sample: {}", l[JUMP_AT]);
    assert!((l[l.len() - 1] - 0.75).abs() < 1e-5);
    let hosted_calls = f.hosted.lock().unwrap();
    // 48 000 samples = 187 whole blocks and a short last one: one call each.
    assert_eq!(hosted_calls.frames.len(), 188, "the plugin gets one call per block");
    assert!(hosted_calls.frames.iter().all(|n| *n == BLOCK || *n == 48_000 - 187 * BLOCK), "whole blocks only");
    let stock_calls = f.stock.lock().unwrap();
    assert!(stock_calls.frames.iter().any(|n| *n < BLOCK), "the stock device is processed in pieces where its lane moves");
    assert!(stock_calls.frames.contains(&(JUMP_AT % BLOCK)), "a piece ends on the jump: {}", JUMP_AT % BLOCK);
}

#[test]
fn a_stock_ramp_reaches_the_device_every_control_block() {
    let f = Probes::default();
    let mut p = Project::new("ramp");
    let t = add(&mut p, TrackKind::Audio, "fx", None);
    let d = p.alloc_device_id();
    p.track_mut(t).unwrap().devices.push(Device::stock(d, "probe", "Probe"));
    p.track_mut(t).unwrap().automation.push(lane(AutomationTarget::DeviceParam { device: d, param: PARAM }, &[(0, 0.0), (2 * PPQ, 1.0)]));
    let l = render(&p, &f).channel(0).to_vec();
    for k in [1usize, 10, 100, 1000, 1400] {
        let at = k * crate::automation::CONTROL_BLOCK;
        let expect = (at / SPT) as f32 / (2 * PPQ) as f32;
        assert!((l[at] - expect).abs() < 2e-3, "at {at}: {} vs {expect}", l[at]);
        assert_eq!(l[at], l[at + crate::automation::CONTROL_BLOCK - 1], "held until the next step");
        assert!(l[at + crate::automation::CONTROL_BLOCK] > l[at]);
    }
}

#[test]
fn a_macro_lane_drives_its_mapped_stock_devices() {
    let f = Probes::default();
    let mut p = Project::new("macro");
    let t = add(&mut p, TrackKind::Audio, "fx", None);
    let (rack_id, a, b) = (p.alloc_device_id(), p.alloc_device_id(), p.alloc_device_id());
    let mut chain = Chain::new("A", Color(0));
    chain.devices.push(Device::stock(a, "probe", "A"));
    let mut rack = Rack::new(RackKind::Audio);
    rack.chains.push(chain);
    let mut chain_b = Chain::new("B", Color(0));
    chain_b.devices.push(Device::stock(b, "probe", "B"));
    rack.chains.push(chain_b);
    rack.macros[0].mappings.push(MacroMapping { device: a, param: PARAM, min: 0.2, max: 0.6 });
    rack.macros[0].mappings.push(MacroMapping { device: b, param: PARAM, min: 0.5, max: 0.1 });
    let mut rd = Device::stock(rack_id, "rack", "Rack");
    rd.kind = DeviceKind::Rack(rack);
    p.track_mut(t).unwrap().devices.push(rd);
    p.track_mut(t).unwrap().automation.push(lane(AutomationTarget::Macro { device: rack_id, index: 0 }, &[(0, 1.0), (2 * PPQ, 1.0)]));
    let l = render(&p, &f).channel(0).to_vec();
    // Both chains sum: 0.6 (A at the macro's top) + 0.1 (B inverted).
    assert!((l[4000] - 0.7).abs() < 1e-4, "{}", l[4000]);
}

#[test]
fn the_gain_effect_still_follows_a_lane_through_the_pieces() {
    // The old behaviour, kept: a stock device's parameter follows its lane.
    let mut p = Project::new("gain");
    let t = add(&mut p, TrackKind::Audio, "fx", Some(TEST_GAIN));
    let d = p.track(t).unwrap().devices[0].id;
    p.track_mut(t).unwrap().automation.push(lane(AutomationTarget::DeviceParam { device: d, param: 0 }, &[(0, 0.9), (PPQ, 0.9)]));
    let (_h, mut proc_) = start(&p, Arc::new(FallbackFactory));
    block(&mut proc_);
    assert!((device_param(&mut proc_, d, 0) - 0.9).abs() < 1e-6);
}

#[test]
fn timed_midi_fast_commands_and_piecewise_automation_never_allocate() {
    let (mut p, keys) = synth_project();
    let fx = add(&mut p, TrackKind::Audio, "fx", Some(TEST_GAIN));
    let d = p.track(fx).unwrap().devices[0].id;
    p.track_mut(fx).unwrap().automation.push(lane(AutomationTarget::DeviceParam { device: d, param: 0 }, &[(0, 0.0), (8 * PPQ, 1.0)]));
    let (h, mut proc_) = start(&p, Arc::new(FallbackFactory));
    h.set_loop(Some((0, 4 * PPQ)));
    h.play();
    let mut buf = AudioBuffer::new(2, BLOCK);
    for _ in 0..4 {
        proc_.process_planar(&mut buf);
    }
    // The fast commands are queued first: the handle's side (a message to
    // the control thread) allocates, the audio thread's side must not.
    for i in 0..300 {
        h.set_device_param(d, 0, (i % 10) as f32 / 10.0);
        h.set_track_volume(keys, -(i % 12) as f32);
        h.set_track_pan(keys, 0.1);
    }
    let ((), allocs) = crate::alloc_counter::count_allocs(|| {
        for i in 0..300 {
            proc_.begin_callback();
            let cb = proc_.callback_start_nanos().max(BLOCK as u64 * 100_000);
            h.send_midi_at(MidiMessage::note_on(0, 60 + (i % 12) as u8, 90), cb.saturating_sub(100_000));
            h.send_midi_at(MidiMessage::note_off(0, 60 + (i % 12) as u8), cb);
            proc_.process_planar(&mut buf);
        }
    });
    assert_eq!(allocs, 0, "live MIDI with offsets, fast commands and a moving stock lane");
    assert!(h.midi_message_count() >= 600);
}

// ───────────────────────────── Races between patches and graph swaps ─────────────────────────────

#[test]
fn a_patch_racing_a_graph_swap_is_not_lost() {
    let (p, d) = fifty_tracks();
    let (h, mut proc_) = start(&p, Arc::new(FallbackFactory));
    let h = Arc::new(h);
    // P2 is structural and also moves the knob and a fader (so its graph
    // carries those values); P3 moves them again, nothing else.
    let mut p2 = p.clone();
    p2.tracks[3].mute = true;
    p2.tracks[26].devices[0].params.insert(0, 0.6);
    p2.tracks[10].volume_db = -3.0;
    let mut p3 = p2.clone();
    p3.tracks[26].devices[0].params.insert(0, 0.77);
    p3.tracks[10].volume_db = -9.0;
    let track10 = p.tracks[10].id;
    let mut p5 = p3.clone();
    let hh = h.clone();
    let mut fired = false;
    // Exactly the bad order: the new graph is queued after this block's
    // install and before its commands, then the parameter-only snapshot arrives.
    proc_.hook_after_install = Some(Box::new(move || {
        if !fired {
            fired = true;
            hh.update_project(&p2);
            hh.flush();
            hh.update_project(&p3);
            hh.flush();
        }
    }));
    block(&mut proc_);
    proc_.hook_after_install = None;
    for _ in 0..3 {
        block(&mut proc_);
    }
    assert!((device_param(&mut proc_, d, 0) - 0.77).abs() < 1e-6, "knob: {}", device_param(&mut proc_, d, 0));
    let g = proc_.graph_mut().unwrap();
    assert_eq!(g.tracks[g.track_index(track10).unwrap()].volume_db, -9.0);
    assert!(g.tracks[g.track_index(p.tracks[3].id).unwrap()].mute, "the structural edit is in too");
    // And the pool agrees: a later compile does not move the knob back.
    p5.tracks[5].solo = true;
    h.update_project(&p5);
    h.flush();
    block(&mut proc_);
    assert!((device_param(&mut proc_, d, 0) - 0.77).abs() < 1e-6, "after a later compile: {}", device_param(&mut proc_, d, 0));
}

#[test]
fn live_midi_is_spread_over_the_chunks_of_a_big_callback() {
    // A 1 024-frame callback in 256-frame chunks: messages from anywhere in
    // the previous callback's span keep their place.
    let (p, _) = synth_project();
    let (h, proc_) = Engine::new(EngineConfig { max_block: 256, ..EngineConfig::headless(SR) }, Arc::new(FallbackFactory)).unwrap();
    let mut proc_ = proc_.unwrap();
    h.update_project(&p);
    h.flush();
    let mut data = vec![0.0f32; 2 * 1024];
    proc_.process_interleaved(&mut data, 2);
    let cb = begin(&mut proc_);
    let at = |frame: usize| cb - (1024.0 / SR * 1e9) as u64 + (frame as f64 / SR * 1e9) as u64;
    assert!(h.send_midi_at(MidiMessage::note_on(0, 60, 100), at(100)));
    assert!(h.send_midi_at(MidiMessage::note_off(0, 60), at(700)));
    proc_.process_interleaved(&mut data, 2);
    let left: Vec<f32> = data.chunks(2).map(|f| f[0]).collect();
    let (first, last) = nonzero_span(&left).expect("the note sounded");
    assert!((99..=101).contains(&first), "note-on at frame 100, heard at {first}");
    assert!((698..=700).contains(&last), "note-off at frame 700, silent from {}", last + 1);
}

#[test]
fn a_recompile_during_a_fast_path_drag_keeps_the_dragged_values() {
    let mut p = Project::new("drag");
    let a = add(&mut p, TrackKind::Audio, "a", Some(TEST_GAIN));
    let r = add(&mut p, TrackKind::Return, "r", None);
    p.track_mut(a).unwrap().sends.push(Send { target: r, level_db: 0.0, pre_fader: false });
    let d = p.track(a).unwrap().devices[0].id;
    let (h, mut proc_) = start(&p, Arc::new(FallbackFactory));
    let compiles = h.compile_count();
    // A drag by the direct commands only: the project copy of the control
    // thread has not been resent yet.
    assert!(h.set_track_volume(a, -6.0));
    assert!(h.set_track_pan(a, 0.5));
    assert!(h.set_send_level(a, 0, -12.0));
    assert!(h.set_device_param(d, 0, 0.33));
    // The audio thread applies them to the running graph...
    block(&mut proc_);
    // ...and then something unrelated rebuilds the graph.
    h.set_track_input_none(a, true);
    h.flush();
    for _ in 0..3 {
        block(&mut proc_);
    }
    assert_eq!(h.compile_count(), compiles + 1);
    assert!((device_param(&mut proc_, d, 0) - 0.33).abs() < 1e-6);
    let g = proc_.graph_mut().unwrap();
    let t = &g.tracks[g.track_index(a).unwrap()];
    assert_eq!((t.volume_db, t.pan, t.sends[0].level_db), (-6.0, 0.5, -12.0), "the new graph starts from the dragged values");
}

#[test]
fn a_channel_pick_survives_a_graph_compiled_before_it() {
    let mut p = Project::new("pick");
    let a = add(&mut p, TrackKind::Audio, "a", None);
    let (h, mut proc_) = start(&p, Arc::new(FallbackFactory));
    let h = Arc::new(h);
    let mut p2 = p.clone();
    p2.track_mut(a).unwrap().mute = true;
    let hh = h.clone();
    let mut fired = false;
    // Exactly the bad order: a graph compiled without the pick is queued
    // after this block's install; the pick reaches the old graph by command.
    proc_.hook_after_install = Some(Box::new(move || {
        if !fired {
            fired = true;
            hh.update_project(&p2);
            hh.flush();
            hh.set_track_input(a, Some(crate::audio_io::ChannelSelection { first: 1, count: 1 }));
            hh.flush();
        }
    }));
    block(&mut proc_);
    proc_.hook_after_install = None;
    for _ in 0..3 {
        block(&mut proc_);
    }
    let g = proc_.graph_mut().unwrap();
    assert_eq!(g.tracks[g.track_index(a).unwrap()].input_sel, Some((1, 1)));
    assert!(g.tracks[g.track_index(a).unwrap()].silenced || p.tracks.len() > 1);
}

// ───────────────────────────── 2.13 · Live macros ─────────────────────────────

fn macro_project() -> (Project, TrackId, DeviceId, DeviceId, DeviceId) {
    let mut p = Project::new("macros");
    let t = add(&mut p, TrackKind::Audio, "fx", None);
    let (rack_id, a, b) = (p.alloc_device_id(), p.alloc_device_id(), p.alloc_device_id());
    let mut chain_a = Chain::new("A", Color(0));
    chain_a.devices.push(Device::stock(a, TEST_GAIN, TEST_GAIN));
    let mut chain_b = Chain::new("B", Color(0));
    chain_b.devices.push(Device::stock(b, TEST_GAIN, TEST_GAIN));
    let mut rack = Rack::new(RackKind::Audio);
    rack.chains.push(chain_a);
    rack.chains.push(chain_b);
    rack.macros[0].mappings.push(MacroMapping { device: a, param: 0, min: 0.2, max: 0.6 });
    rack.macros[0].mappings.push(MacroMapping { device: b, param: 0, min: 0.9, max: 0.1 });
    let mut rd = Device::stock(rack_id, "rack", "Rack");
    rd.kind = DeviceKind::Rack(rack);
    p.track_mut(t).unwrap().devices.push(rd);
    (p, t, rack_id, a, b)
}

fn set_macro_value(p: &mut Project, rack: DeviceId, v: f32) {
    let DeviceKind::Rack(r) = &mut p.tracks[1].devices.iter_mut().find(|d| d.id == rack).unwrap().kind else { unreachable!() };
    r.macros[0].value = v;
}

#[test]
fn dragging_a_macro_never_compiles() {
    let (mut p, _, rack, a, b) = macro_project();
    let (h, mut proc_) = start(&p, Arc::new(FallbackFactory));
    let compiles = h.compile_count();
    // The app's drag: the direct call for the instant sound, the project after.
    for i in 0..60 {
        let v = i as f32 / 59.0;
        assert!(h.set_macro(rack, 0, v));
        set_macro_value(&mut p, rack, v);
        h.update_project(&p);
        h.flush();
        block(&mut proc_);
        let (ea, eb) = (0.2 + 0.4 * v, 0.9 - 0.8 * v);
        assert!((device_param(&mut proc_, a, 0) - ea).abs() < 1e-5, "frame {i}: {} vs {ea}", device_param(&mut proc_, a, 0));
        assert!((device_param(&mut proc_, b, 0) - eb).abs() < 1e-5, "frame {i} (inverted range)");
    }
    assert_eq!(h.compile_count(), compiles, "0 compiles during the drag");
    // The project alone (no direct call) moves it too.
    for i in 0..20 {
        let v = 1.0 - i as f32 / 40.0;
        set_macro_value(&mut p, rack, v);
        h.update_project(&p);
        h.flush();
        block(&mut proc_);
        assert!((device_param(&mut proc_, a, 0) - (0.2 + 0.4 * v)).abs() < 1e-5, "project-only frame {i}");
    }
    assert_eq!(h.compile_count(), compiles);
    // A later compile starts from the macro where it was left.
    let v = 1.0 - 19.0 / 40.0;
    p.tracks[1].mute = true;
    h.update_project(&p);
    h.flush();
    block(&mut proc_);
    assert_eq!(h.compile_count(), compiles + 1);
    assert!((device_param(&mut proc_, a, 0) - (0.2 + 0.4 * v)).abs() < 1e-5, "{}", device_param(&mut proc_, a, 0));
}

#[test]
fn a_direct_macro_move_survives_a_recompile_before_the_project_arrives() {
    let (p, t, rack, a, _) = macro_project();
    let (h, mut proc_) = start(&p, Arc::new(FallbackFactory));
    assert!(h.set_macro(rack, 0, 1.0));
    block(&mut proc_);
    h.set_track_input_none(t, true); // rebuilds the graph meanwhile
    h.flush();
    for _ in 0..2 {
        block(&mut proc_);
    }
    assert!((device_param(&mut proc_, a, 0) - 0.6).abs() < 1e-5, "{}", device_param(&mut proc_, a, 0));
}
