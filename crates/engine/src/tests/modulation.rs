//! Modulators: a `mod.*` device's output is added to its mapped parameters
//! (and the fader) on top of the knob's value and the automation, as timed
//! events and control-step offsets, the same live and in an offline render.

use crate::automation::CONTROL_BLOCK;
use crate::export::render_offline;
use crate::factory::ProcessorFactory;
use crate::fallback::{FallbackFactory, TEST_SYNTH};
use aitrack_dsp::{AudioBuffer, Category, EventKind, ModBlock, ParamInfo, PluginInfo, ProcessContext, Processor};
use aitrack_model::*;
use std::path::PathBuf;

const SR: f64 = 48_000.0;
const BEAT: usize = 24_000;
const PARAM: u32 = 7;

/// Writes its parameter's value into every sample (changing at each `Param` event).
struct Probe {
    info: PluginInfo,
    value: f32,
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
            while let Some((_, v)) = next.filter(|(off, _)| *off <= i) {
                self.value = v;
                next = events.next();
            }
            l[i] = self.value;
            r[i] = self.value;
        }
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

/// `mod.saw`: the beat's fraction (0..1 per beat); `mod.one`: constant 1.
struct TestMod {
    info: PluginInfo,
    saw: bool,
}

impl Processor for TestMod {
    fn info(&self) -> &PluginInfo {
        &self.info
    }
    fn prepare(&mut self, _: f64, _: usize) {}
    fn reset(&mut self) {}
    fn process(&mut self, _: &mut ProcessContext<'_>) {}
    fn modulate(&mut self, m: &ModBlock<'_>, out: &mut [f32]) -> bool {
        for (k, o) in out.iter_mut().enumerate() {
            *o = if self.saw { m.beats[k].fract() as f32 } else { 1.0 };
        }
        true
    }
    fn set_param(&mut self, _: u32, _: f32) {}
    fn get_param(&self, _: u32) -> f32 {
        0.0
    }
}

struct Factory;

impl ProcessorFactory for Factory {
    fn create(&self, d: &Device, sr: f64, max: usize) -> Option<Box<dyn Processor>> {
        let info = |id: &'static str, category| PluginInfo {
            id,
            name: id,
            vendor: "AI-Track",
            category,
            tags: &[],
            params: vec![ParamInfo::new(PARAM, "Value", 0.0, 1.0, 0.0)],
            inputs: 2,
            outputs: 2,
            has_sidechain: false,
        };
        match &d.kind {
            DeviceKind::Stock { plugin_id } if plugin_id == "mod.saw" => Some(Box::new(TestMod { info: info("mod.saw", Category::Utility), saw: true })),
            DeviceKind::Stock { plugin_id } if plugin_id == "mod.one" => Some(Box::new(TestMod { info: info("mod.one", Category::Utility), saw: false })),
            DeviceKind::Plugin { .. } => Some(Box::new(Probe { info: info("probe", Category::Effect), value: 0.0 })),
            _ => FallbackFactory.create(d, sr, max),
        }
    }
}

fn probe(id: DeviceId, base: f32) -> Device {
    let mut d = Device::stock(id, "probe", "Probe");
    d.kind = DeviceKind::Plugin { format: PluginFormat::Clap, uid: "x".into(), path: PathBuf::from("/probe.clap"), vendor: "x".into(), is_instrument: false };
    d.params.insert(PARAM, base);
    d
}

fn modulator(id: DeviceId, plugin: &str, mods: Vec<ModMapping>) -> Device {
    Device { mods, ..Device::stock(id, plugin, plugin) }
}

fn map(device: DeviceId, min: f32, max: f32) -> ModMapping {
    ModMapping { target: ModTarget::Param { device, param: PARAM }, min, max }
}

fn audio_track(devices: Vec<Device>) -> Project {
    let mut p = Project::new("mod");
    let id = p.alloc_track_id();
    let mut t = Track::new(id, TrackKind::Audio, "fx", Color(0));
    t.devices = devices;
    p.tracks.push(t);
    p
}

fn render(p: &Project, block: usize) -> AudioBuffer {
    render_offline(p, &Factory, (0, 2 * PPQ), SR, block).unwrap()
}

#[test]
fn a_modulator_adds_its_range_on_top_of_the_knob() {
    let p = audio_track(vec![probe(DeviceId(1), 0.25), modulator(DeviceId(2), "mod.saw", vec![map(DeviceId(1), -0.1, 0.3)])]);
    let out = render(&p, 256);
    let l = out.channel(0);
    for i in [0usize, 31, 32, 6_000, 12_000 + 5, BEAT - 1, BEAT, BEAT + 700] {
        let step_start = (i / CONTROL_BLOCK * CONTROL_BLOCK) as f64;
        let saw = (step_start / BEAT as f64).fract() as f32;
        let want = 0.25 + (-0.1 + 0.4 * saw);
        assert!((l[i] - want).abs() < 1e-4, "sample {i}: {} vs {want}", l[i]);
    }
    // The knob is the base: without the mapping the parameter just holds it.
    let mut off = p.clone();
    off.tracks.last_mut().unwrap().devices[1].mods.clear();
    assert!(render(&off, 256).channel(0).iter().all(|v| (*v - 0.25).abs() < 1e-6 || *v == 0.0), "unmapped = base");
    assert!((render(&off, 256).channel(0)[BEAT] - 0.25).abs() < 1e-6);
}

#[test]
fn the_render_does_not_depend_on_the_block_size() {
    let p = audio_track(vec![probe(DeviceId(1), 0.5), modulator(DeviceId(2), "mod.saw", vec![map(DeviceId(1), 0.0, 0.4)])]);
    let a = render(&p, 256);
    let b = render(&p, 512);
    let c = render(&p, 128);
    assert_eq!(a.channel(0), b.channel(0));
    assert_eq!(a.channel(0), c.channel(0));
}

#[test]
fn automation_keeps_driving_the_base_underneath() {
    let mut p = audio_track(vec![probe(DeviceId(1), 0.1), modulator(DeviceId(2), "mod.one", vec![map(DeviceId(1), 0.0, 0.2)])]);
    let mut lane = AutomationLane::new(AutomationTarget::DeviceParam { device: DeviceId(1), param: PARAM });
    lane.insert(AutomationPoint { tick: 0, value: 0.3, curve: 0.0 });
    lane.insert(AutomationPoint { tick: PPQ, value: 0.3, curve: 0.0 });
    lane.insert(AutomationPoint { tick: PPQ + 1, value: 0.6, curve: 0.0 });
    p.tracks.last_mut().unwrap().automation.push(lane);
    let l = render(&p, 256);
    let l = l.channel(0);
    assert!((l[1000] - 0.5).abs() < 1e-5, "0.3 automated + 0.2 modulation: {}", l[1000]);
    assert!((l[BEAT + 2000] - 0.8).abs() < 1e-5, "0.6 + 0.2 after the jump: {}", l[BEAT + 2000]);
}

#[test]
fn offsets_clamp_to_the_parameter_range() {
    let p = audio_track(vec![probe(DeviceId(1), 0.9), modulator(DeviceId(2), "mod.one", vec![map(DeviceId(1), 0.0, 0.5)])]);
    assert!(render(&p, 256).channel(0)[5000] <= 1.0);
    let q = audio_track(vec![probe(DeviceId(1), 0.1), modulator(DeviceId(2), "mod.one", vec![map(DeviceId(1), -0.5, -0.5)])]);
    assert_eq!(render(&q, 256).channel(0)[5000], 0.0);
}

#[test]
fn a_disabled_modulator_leaves_the_base_alone() {
    let mut p = audio_track(vec![probe(DeviceId(1), 0.25), modulator(DeviceId(2), "mod.one", vec![map(DeviceId(1), 0.2, 0.2)])]);
    assert!((render(&p, 256).channel(0)[5000] - 0.45).abs() < 1e-5);
    p.tracks.last_mut().unwrap().devices[1].enabled = false;
    assert!((render(&p, 256).channel(0)[5000] - 0.25).abs() < 1e-5);
}

#[test]
fn the_fader_can_be_modulated() {
    let mut p = Project::new("mod");
    let t = p.alloc_track_id();
    p.tracks.push(Track::new(t, TrackKind::Instrument, "synth", Color(0)));
    let (synth, m) = (p.alloc_device_id(), p.alloc_device_id());
    p.track_mut(t).unwrap().devices.push(Device::stock(synth, TEST_SYNTH, "synth"));
    p.track_mut(t).unwrap().devices.push(modulator(m, "mod.one", vec![]));
    let sid = p.alloc_source_id();
    let mut src = NoteSource::default();
    let nid = p.alloc_note_id();
    src.notes.push(Note::new(nid, 60, 0, 2 * PPQ, 1.0));
    p.sources.insert(sid, Source::Notes(src));
    let cid = p.alloc_clip_id();
    p.clips.insert(cid, Clip::new("c", Color(0), sid, 2 * PPQ));
    let pid = p.alloc_placement_id();
    p.track_mut(t).unwrap().arrangement.push(Placement { id: pid, clip: cid, start: 0, lane: 0 });
    let peak = |p: &Project| render(p, 256).channel(0)[BEAT..BEAT + 4000].iter().fold(0.0f32, |a, s| a.max(s.abs()));
    let plain = peak(&p);
    assert!(plain > 0.01);
    // The output (1.0) puts +offset on the normalised fader: -0.1 is about -7.6 dB from unity.
    p.track_mut(t).unwrap().devices[1].mods = vec![ModMapping { target: ModTarget::Volume, min: -0.1, max: -0.1 }];
    let lowered = peak(&p);
    let db = 20.0 * (lowered / plain).log10();
    assert!((db - -7.6).abs() < 0.3, "{db} dB");
    // Pan: a full left offset empties the right channel.
    p.track_mut(t).unwrap().devices[1].mods = vec![ModMapping { target: ModTarget::Pan, min: -0.5, max: -0.5 }];
    let out = render(&p, 256);
    let r = out.channel(1)[BEAT..BEAT + 4000].iter().fold(0.0f32, |a, s| a.max(s.abs()));
    assert!(r < 1e-3 && out.channel(0)[BEAT..BEAT + 4000].iter().any(|s| s.abs() > 0.01));
}

#[test]
fn a_mapping_to_a_missing_device_is_ignored() {
    let p = audio_track(vec![probe(DeviceId(1), 0.25), modulator(DeviceId(2), "mod.one", vec![map(DeviceId(99), 0.0, 0.5), map(DeviceId(1), 0.1, 0.1)])]);
    assert!((render(&p, 256).channel(0)[5000] - 0.35).abs() < 1e-5);
}

#[test]
fn a_live_knob_is_the_new_base_a_range_edit_applies_and_nothing_allocates() {
    use crate::alloc_counter::count_allocs;
    use crate::{Engine, EngineConfig};
    use std::sync::Arc;
    let p = audio_track(vec![probe(DeviceId(1), 0.25), modulator(DeviceId(2), "mod.one", vec![map(DeviceId(1), 0.2, 0.2)])]);
    let (h, proc_) = Engine::new(EngineConfig::headless(SR), Arc::new(Factory)).unwrap();
    let mut proc_ = proc_.unwrap();
    h.update_project(&p);
    h.flush();
    h.play();
    let mut buf = AudioBuffer::new(2, 256);
    for _ in 0..4 {
        proc_.process_planar(&mut buf);
    }
    assert!((buf.channel(0)[255] - 0.45).abs() < 1e-4, "{}", buf.channel(0)[255]);
    // The knob moves: the device gets the new base plus the modulation.
    h.set_device_param(DeviceId(1), PARAM, 0.5);
    h.flush();
    for _ in 0..2 {
        proc_.process_planar(&mut buf);
    }
    assert!((buf.channel(0)[255] - 0.7).abs() < 1e-4, "{}", buf.channel(0)[255]);
    // The range is edited in the project (a patch, not a compile).
    let mut q = p.clone();
    q.tracks.last_mut().unwrap().devices[0].params.insert(PARAM, 0.5);
    q.tracks.last_mut().unwrap().devices[1].mods[0] = map(DeviceId(1), 0.4, 0.4);
    h.update_project(&q);
    h.flush();
    for _ in 0..4 {
        proc_.process_planar(&mut buf);
    }
    assert!((buf.channel(0)[255] - 0.9).abs() < 1e-4, "{}", buf.channel(0)[255]);
    let (_, n) = count_allocs(|| {
        for _ in 0..300 {
            proc_.process_planar(&mut buf);
        }
    });
    assert_eq!(n, 0, "modulating allocates nothing");
}
