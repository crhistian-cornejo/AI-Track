//! Automation of a hosted plugin's parameters reaches the processor as timed
//! `Param` events inside the block (what the VST3 and CLAP hosts turn into
//! `IParameterChanges` points and param-value events with their time), not
//! by cutting the chain into control blocks: the processor is called once
//! per block and a drawn jump lands on its exact sample.

use crate::automation::CONTROL_BLOCK;
use crate::export::render_offline;
use crate::factory::ProcessorFactory;
use aitrack_dsp::{AudioBuffer, Category, EventKind, ParamInfo, PluginInfo, ProcessContext, Processor};
use aitrack_model::*;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;
/// Samples per tick at 120 BPM and 48 kHz.
const SPT: usize = 25;
const PARAM: u32 = 7;

/// What the probes saw: the frames of every process call and the values
/// set through `set_param` (the control-block path).
#[derive(Default)]
struct Seen {
    frames: Vec<usize>,
    set: usize,
}

/// Writes its parameter's current value into every sample, changing it at
/// the offset of each `Param` event, so the output shows where each event
/// landed.
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
        let n = ctx.buffer.frames();
        let mut events = ctx.events.iter().filter_map(|e| match e.kind {
            EventKind::Param { id: PARAM, value } => Some((e.offset as usize, value)),
            _ => None,
        });
        let mut next = events.next();
        let (l, r) = ctx.buffer.stereo_mut();
        for i in 0..n {
            while let Some((off, v)) = next.filter(|(off, _)| *off <= i) {
                assert!(off < n, "event offset {off} outside a {n}-frame call");
                self.value = v;
                next = events.next();
            }
            l[i] = self.value;
            r[i] = self.value;
        }
        if let Ok(mut s) = self.seen.try_lock() {
            s.frames.push(n);
        }
    }
    fn set_param(&mut self, id: u32, v: f32) {
        if id == PARAM {
            self.value = v;
            if let Ok(mut s) = self.seen.try_lock() {
                s.set += 1;
            }
        }
    }
    fn get_param(&self, id: u32) -> f32 {
        if id == PARAM { self.value } else { 0.0 }
    }
}

#[derive(Default)]
struct Probes {
    seen: Arc<Mutex<Seen>>,
}

impl ProcessorFactory for Probes {
    fn create(&self, _: &Device, _: f64, _: usize) -> Option<Box<dyn Processor>> {
        let info = PluginInfo {
            id: "probe",
            name: "Probe",
            vendor: "AI-Track",
            category: Category::Effect,
            tags: &[],
            params: vec![ParamInfo::new(PARAM, "Value", 0.0, 1.0, 0.0), ParamInfo::new(0, "Other", 0.0, 1.0, 0.0)],
            inputs: 2,
            outputs: 2,
            has_sidechain: false,
        };
        Some(Box::new(Probe { info, value: 0.0, seen: self.seen.clone() }))
    }
}

fn plugin(id: DeviceId) -> Device {
    let mut d = Device::stock(id, "probe", "Probe");
    d.kind = DeviceKind::Plugin { format: PluginFormat::Clap, uid: "com.example.probe".into(), path: PathBuf::from("/probe.clap"), vendor: "Example".into(), is_instrument: false };
    d
}

fn pt(tick: Tick, value: f32) -> AutomationPoint {
    AutomationPoint { tick, value, curve: 0.0 }
}

/// An audio track (silent input) whose chain is `devices`, with `lanes`.
fn project(devices: Vec<Device>, lanes: Vec<AutomationLane>) -> Project {
    let mut p = Project::new("auto");
    let id = p.alloc_track_id();
    let mut t = Track::new(id, TrackKind::Audio, "fx", Color(0));
    t.devices = devices;
    t.automation = lanes;
    p.tracks.push(t);
    p
}

fn lane(device: DeviceId, points: Vec<AutomationPoint>) -> AutomationLane {
    AutomationLane { points, ..AutomationLane::new(AutomationTarget::DeviceParam { device, param: PARAM }) }
}

/// A jump from 0.25 to 0.75 one tick after the first beat: sample 24 025,
/// which is on neither a block nor a control-block boundary.
fn jump() -> Vec<AutomationPoint> {
    vec![pt(0, 0.25), pt(PPQ + 1, 0.25), pt(PPQ + 1, 0.75)]
}
const JUMP_AT: usize = (PPQ as usize + 1) * SPT;

fn render(p: &Project, f: &Probes) -> AudioBuffer {
    render_offline(p, f, (0, 2 * PPQ), SR, BLOCK).unwrap()
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-5
}

#[test]
fn plugin_automation_lands_on_its_exact_sample_in_whole_blocks() {
    assert_ne!(JUMP_AT % CONTROL_BLOCK, 0);
    assert_ne!(JUMP_AT % BLOCK, 0);
    let f = Probes::default();
    let mut p = project(Vec::new(), Vec::new());
    let d = p.alloc_device_id();
    let t = p.tracks[1].id;
    p.track_mut(t).unwrap().devices.push(plugin(d));
    p.track_mut(t).unwrap().automation.push(lane(d, jump()));
    let out = render(&p, &f);
    let l = out.channel(0);
    // (The fader fades in over the first control block.)
    assert!(close(l[CONTROL_BLOCK], 0.25), "the value from the start: {}", l[CONTROL_BLOCK]);
    assert!(close(l[JUMP_AT - 1], 0.25), "before the jump: {}", l[JUMP_AT - 1]);
    assert!(close(l[JUMP_AT], 0.75), "on the jump's sample: {}", l[JUMP_AT]);
    assert!(close(l[l.len() - 1], 0.75));
    let seen = f.seen.lock().unwrap();
    assert!(!seen.frames.contains(&CONTROL_BLOCK), "called in whole blocks, never cut into control blocks: {:?}", &seen.frames[..8.min(seen.frames.len())]);
    assert_eq!(seen.set, 0, "automation never goes through set_param");
}

#[test]
fn a_ramp_reaches_the_plugin_every_control_block() {
    let f = Probes::default();
    let d = DeviceId(9_000);
    let p = project(vec![plugin(d)], vec![lane(d, vec![pt(0, 0.0), pt(2 * PPQ, 1.0)])]);
    let out = render(&p, &f);
    let l = out.channel(0);
    for k in [1usize, 10, 100, 1000, 1400] {
        let at = k * CONTROL_BLOCK;
        let expect = (at / SPT) as f32 / (2 * PPQ) as f32;
        assert!((l[at] - expect).abs() < 2e-3, "at {at}: {} vs {expect}", l[at]);
        // Held until the next control step.
        assert_eq!(l[at], l[at + CONTROL_BLOCK - 1]);
        assert!(l[at + CONTROL_BLOCK] > l[at], "rising at {at}");
    }
}

#[test]
fn a_plugin_inside_a_rack_gets_its_events() {
    let f = Probes::default();
    let (rack_id, d) = (DeviceId(9_001), DeviceId(9_002));
    let mut chain = Chain::new("A", Color(0));
    chain.devices.push(plugin(d));
    let mut rack = Rack::new(RackKind::Audio);
    rack.chains.push(chain);
    let mut rd = Device::stock(rack_id, "rack", "Rack");
    rd.kind = DeviceKind::Rack(rack);
    let p = project(vec![rd], vec![lane(d, jump())]);
    let l = render(&p, &f).channel(0).to_vec();
    assert!(close(l[JUMP_AT - 1], 0.25) && close(l[JUMP_AT], 0.75), "{} {}", l[JUMP_AT - 1], l[JUMP_AT]);
    assert!(!f.seen.lock().unwrap().frames.contains(&CONTROL_BLOCK));
}

#[test]
fn next_to_control_block_automation_the_events_keep_their_offsets() {
    // A stock parameter lane on the same track cuts the chain into control
    // blocks; the plugin's events are cut with it and still land exactly.
    let f = Probes::default();
    let (stock, d) = (DeviceId(9_003), DeviceId(9_004));
    let other = AutomationLane { points: vec![pt(0, 0.5)], ..AutomationLane::new(AutomationTarget::DeviceParam { device: stock, param: 0 }) };
    let p = project(vec![Device::stock(stock, "probe", "Probe"), plugin(d)], vec![other, lane(d, jump())]);
    let l = render(&p, &f).channel(0).to_vec();
    assert!(close(l[JUMP_AT - 1], 0.25), "{}", l[JUMP_AT - 1]);
    assert!(close(l[JUMP_AT], 0.75), "{}", l[JUMP_AT]);
    assert!(f.seen.lock().unwrap().frames.contains(&CONTROL_BLOCK), "this track is cut into control blocks");
}
