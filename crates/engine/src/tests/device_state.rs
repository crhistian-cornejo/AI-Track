//! Device state reaching live processors: a new `Device::state` rebuilds the
//! processor off the audio thread, identical bytes are a no-op, and a plugin
//! writing its own state back into the project is never rebuilt.

use crate::compile::{compile, CompileContext};
use crate::factory::ProcessorFactory;
use crate::graph::DeviceNode;
use crate::sample_cache::SampleCache;
use crate::shared::{ProcessorPool, SharedProcessor, StateEchoes};
use crate::CompiledGraph;
use aitrack_dsp::{Category, ParamInfo, PluginInfo, ProcessContext, Processor};
use aitrack_model::*;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// Every processor it builds logs `create` and each `load_state` here.
#[derive(Default)]
struct Recording {
    log: Arc<Mutex<Vec<String>>>,
}

struct Rec {
    info: PluginInfo,
    log: Arc<Mutex<Vec<String>>>,
    state: Vec<u8>,
    gain: f32,
}

impl Processor for Rec {
    fn info(&self) -> &PluginInfo {
        &self.info
    }
    fn prepare(&mut self, _: f64, _: usize) {}
    fn reset(&mut self) {}
    fn process(&mut self, _: &mut ProcessContext<'_>) {}
    fn set_param(&mut self, id: u32, v: f32) {
        if id == 0 {
            self.gain = v;
        }
    }
    fn get_param(&self, id: u32) -> f32 {
        if id == 0 { self.gain } else { 0.0 }
    }
    fn save_state(&self) -> Vec<u8> {
        self.state.clone()
    }
    fn load_state(&mut self, data: &[u8]) {
        self.state = data.to_vec();
        self.log.lock().unwrap().push(format!("load {}", String::from_utf8_lossy(data)));
    }
}

impl ProcessorFactory for Recording {
    fn create(&self, device: &Device, _: f64, _: usize) -> Option<Box<dyn Processor>> {
        self.log.lock().unwrap().push("create".into());
        let info = PluginInfo {
            id: "rec",
            name: "Rec",
            vendor: "AI-Track",
            category: Category::Effect,
            tags: &[],
            params: vec![ParamInfo::new(0, "Gain", 0.0, 1.0, 0.5)],
            inputs: 2,
            outputs: 2,
            has_sidechain: false,
        };
        let _ = device;
        Some(Box::new(Rec { info, log: self.log.clone(), state: Vec::new(), gain: 0.5 }))
    }
}

struct Rig {
    p: Project,
    track: TrackId,
    device: DeviceId,
    pool: ProcessorPool,
    echoes: Arc<StateEchoes>,
    factory: Recording,
    samples: SampleCache,
}

impl Rig {
    fn new(kind: DeviceKind, state: Option<&[u8]>) -> Self {
        let mut p = Project::new("state");
        let track = p.alloc_track_id();
        p.tracks.push(Track::new(track, TrackKind::Audio, "fx", Color(0)));
        let device = p.alloc_device_id();
        let mut d = Device::stock(device, "rec", "Rec");
        d.kind = kind;
        d.state = state.map(<[u8]>::to_vec);
        p.track_mut(track).unwrap().devices.push(d);
        let echoes = Arc::new(StateEchoes::default());
        Self { p, track, device, pool: ProcessorPool::with_echoes(echoes.clone()), echoes, factory: Recording::default(), samples: SampleCache::blocking() }
    }

    fn compile(&mut self) -> CompiledGraph {
        compile(
            &self.p,
            &mut CompileContext {
                factory: &self.factory,
                pool: &mut self.pool,
                samples: &self.samples,
                sample_rate: 48_000.0,
                max_frames: 256,
                prev_meters: None,
                taps: Vec::new(),
                analysis: None,
            },
        )
        .unwrap()
    }

    fn proc_of(&self, g: &CompiledGraph) -> (Arc<SharedProcessor>, Vec<(u32, f32)>) {
        match &g.tracks[g.track_index(self.track).unwrap()].devices[0] {
            DeviceNode::Proc(p) => (p.proc.clone(), p.init_params.clone()),
            _ => panic!("not a processor"),
        }
    }

    fn set_state(&mut self, state: Option<&[u8]>) {
        Command::SetDeviceState { device: self.device, state: state.map(<[u8]>::to_vec), preset: None }.apply(&mut self.p).unwrap();
    }

    fn take_log(&self) -> Vec<String> {
        std::mem::take(&mut *self.factory.log.lock().unwrap())
    }

    /// The state the live processor holds. Only valid while no audio thread
    /// runs these graphs (true in these tests).
    fn live_state(&self, proc: &SharedProcessor) -> Vec<u8> {
        // SAFETY: nothing else uses the processor in this test.
        unsafe { proc.get_mut() }.save_state()
    }
}

fn plugin_kind() -> DeviceKind {
    DeviceKind::Plugin { format: PluginFormat::Clap, uid: "com.example.rec".into(), path: PathBuf::from("/x.clap"), vendor: "Example".into(), is_instrument: false }
}

#[test]
fn a_new_device_state_reaches_a_stock_processor() {
    let mut r = Rig::new(DeviceKind::Stock { plugin_id: "rec".into() }, Some(b"saw"));
    r.p.device_mut(r.device).unwrap().params.insert(0, 0.75);
    let g1 = r.compile();
    let (p1, _) = r.proc_of(&g1);
    assert_eq!(r.take_log(), ["create", "load saw"]);
    // Import a new wavetable: the processor is rebuilt with it, off the
    // audio thread, and keeps every parameter.
    r.set_state(Some(b"user table"));
    let g2 = r.compile();
    let (p2, init) = r.proc_of(&g2);
    assert!(!Arc::ptr_eq(&p1, &p2), "a fresh processor carries the new state");
    assert_eq!(r.take_log(), ["create", "load user table"]);
    assert_eq!(r.live_state(&p2), b"user table");
    assert!(init.is_empty(), "a rebuilt processor gets its params up front");
    // SAFETY: as above.
    assert_eq!(unsafe { p2.get_mut() }.get_param(0), 0.75);
    // The old graph still owns the old processor until it is retired.
    assert_eq!(r.live_state(&p1), b"saw");
    drop(g1);
    assert_eq!(Arc::strong_count(&p1), 1, "only the test's clone is left");
    // Same meter cell, so the UI keeps reading the device.
    let track = r.track;
    let meter = move |g: &CompiledGraph| match &g.tracks[g.track_index(track).unwrap()].devices[0] {
        DeviceNode::Proc(p) => p.meter.clone(),
        _ => panic!(),
    };
    let g3 = r.compile();
    assert!(Arc::ptr_eq(&meter(&g2), &meter(&g3)));
    // Clearing the state rebuilds it without loading anything.
    r.set_state(None);
    let g4 = r.compile();
    assert!(!Arc::ptr_eq(&p2, &r.proc_of(&g4).0));
    assert_eq!(r.take_log(), ["create"]);
}

#[test]
fn identical_state_is_a_no_op() {
    let mut r = Rig::new(DeviceKind::Stock { plugin_id: "rec".into() }, Some(b"saw"));
    let g1 = r.compile();
    r.take_log();
    // Same bytes again (a preset reloaded, an undo back to the same state).
    r.set_state(Some(b"saw"));
    r.p.device_mut(r.device).unwrap().params.insert(0, 0.25);
    let g2 = r.compile();
    let (p2, init) = r.proc_of(&g2);
    assert!(Arc::ptr_eq(&r.proc_of(&g1).0, &p2));
    assert_eq!(init, vec![(0, 0.25)], "only the changed param is sent");
    assert!(r.take_log().is_empty(), "nothing created or reloaded");
    let g3 = r.compile();
    assert!(Arc::ptr_eq(&p2, &r.proc_of(&g3).0));
    assert!(r.take_log().is_empty());
}

#[test]
fn a_plugin_writing_its_own_state_back_is_not_rebuilt() {
    let mut r = Rig::new(plugin_kind(), None);
    let g1 = r.compile();
    let (p1, _) = r.proc_of(&g1);
    assert_eq!(r.take_log(), ["create"]);
    // The user tweaks the plugin in its own editor, then saves: the app
    // captures the plugin's state, notes it, and writes it into the project.
    let captured = b"knob at 3 o'clock".to_vec();
    r.echoes.note(r.device, &captured);
    // A compile for an unrelated edit before the echo arrives keeps the note.
    r.p.track_mut(r.track).unwrap().volume_db = -6.0;
    let g2 = r.compile();
    assert!(Arc::ptr_eq(&p1, &r.proc_of(&g2).0));
    r.set_state(Some(&captured));
    let g3 = r.compile();
    assert!(Arc::ptr_eq(&p1, &r.proc_of(&g3).0), "the echo keeps the live plugin");
    assert!(r.take_log().is_empty(), "no rebuild, no load_state");
    // The note is used up: the bytes now count as applied, and a real
    // change (another preset) still reaches the plugin.
    let g4 = r.compile();
    assert!(Arc::ptr_eq(&p1, &r.proc_of(&g4).0));
    r.set_state(Some(b"factory preset"));
    let g5 = r.compile();
    assert!(!Arc::ptr_eq(&p1, &r.proc_of(&g5).0));
    assert_eq!(r.take_log(), ["create", "load factory preset"]);
}

#[test]
fn a_stale_note_does_not_hide_a_later_change() {
    let mut r = Rig::new(plugin_kind(), Some(b"a"));
    let g1 = r.compile();
    let (p1, _) = r.proc_of(&g1);
    r.take_log();
    // Captured bytes equal to what the project already has: the app sends
    // no command, so the note must not linger.
    r.echoes.note(r.device, b"a");
    r.compile();
    r.set_state(Some(b"b"));
    let g2 = r.compile();
    let (p2, _) = r.proc_of(&g2);
    assert!(!Arc::ptr_eq(&p1, &p2));
    r.take_log();
    // Undo back to "a": a real change, so it must be applied.
    r.set_state(Some(b"a"));
    let g3 = r.compile();
    assert!(!Arc::ptr_eq(&p2, &r.proc_of(&g3).0));
    assert_eq!(r.take_log(), ["create", "load a"]);
}
