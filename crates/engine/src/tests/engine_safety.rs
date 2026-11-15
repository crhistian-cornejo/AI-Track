//! The audio path survives faults and compiles what it is given: a panic in
//! a processor or in the callback becomes silence and a flag (0.1), the
//! output bus never carries a NaN or an infinity (0.13), a project with a
//! broken route or a routing cycle still compiles and plays and says what it
//! changed (0.2), new graphs start with their faders at their targets (0.6)
//! and a parameter whose automation is gone returns to its knob (0.12).

use crate::alloc_counter::count_allocs;
use crate::compile::{compile, CompileContext};
use crate::export::{render_offline, render_offline_with};
use crate::factory::ProcessorFactory;
use crate::fallback::{FallbackFactory, TEST_GAIN};
use crate::graph::find_proc_mut;
use crate::sample_cache::{DecodedAudio, SampleCache};
use crate::shared::ProcessorPool;
use crate::{AudioProcessor, Engine, EngineConfig};
use aitrack_dsp::{AudioBuffer, Category, ParamInfo, PluginInfo, ProcessContext, Processor};
use aitrack_model::*;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;

const PANIC: &str = "bomb.panic";
const NAN: &str = "bomb.nan";
const DC: &str = "bomb.dc";
const PANIC_IN_LATENCY: &str = "bomb.latency";
const PANIC_IN_SET_PARAM: &str = "bomb.setparam";

/// A processor that writes a constant `0.25` and then misbehaves as asked:
/// panics, or leaves a NaN / an infinity in the buffer.
struct Bomb {
    info: PluginInfo,
    id: &'static str,
    calls: Arc<AtomicUsize>,
    /// `set_param` panics while this is raised (set after the first compile,
    /// which calls it on the control thread).
    armed: Arc<AtomicBool>,
}

impl Processor for Bomb {
    fn info(&self) -> &PluginInfo {
        &self.info
    }
    fn prepare(&mut self, _: f64, _: usize) {}
    fn reset(&mut self) {}
    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let (l, r) = ctx.buffer.stereo_mut();
        l.fill(0.25);
        r.fill(0.25);
        match self.id {
            PANIC => panic!("injected panic in process"),
            NAN => {
                l[3] = f32::NAN;
                l[10] = f32::INFINITY;
                r[7] = f32::NEG_INFINITY;
                r[8] = f32::NAN;
            }
            _ => {}
        }
    }
    fn latency(&self) -> u32 {
        if self.id == PANIC_IN_LATENCY && self.calls.load(Ordering::Relaxed) > 0 {
            panic!("injected panic in latency");
        }
        0
    }
    fn set_param(&mut self, _: u32, _: f32) {
        if self.id == PANIC_IN_SET_PARAM && self.armed.load(Ordering::Relaxed) {
            panic!("injected panic in set_param");
        }
    }
    fn get_param(&self, _: u32) -> f32 {
        0.0
    }
}

#[derive(Default)]
struct Bombs {
    calls: Arc<AtomicUsize>,
    armed: Arc<AtomicBool>,
}

impl ProcessorFactory for Bombs {
    fn create(&self, device: &Device, _: f64, _: usize) -> Option<Box<dyn Processor>> {
        let DeviceKind::Stock { plugin_id } = &device.kind else { return None };
        let id = [PANIC, NAN, DC, PANIC_IN_LATENCY, PANIC_IN_SET_PARAM].into_iter().find(|k| *k == plugin_id)?;
        let info = PluginInfo {
            id,
            name: "Bomb",
            vendor: "AI-Track",
            category: Category::Effect,
            tags: &[],
            params: vec![ParamInfo::new(0, "Unused", 0.0, 1.0, 0.0)],
            inputs: 2,
            outputs: 2,
            has_sidechain: false,
        };
        Some(Box::new(Bomb { info, id, calls: self.calls.clone(), armed: self.armed.clone() }))
    }
}

/// One audio track per plugin id, each with one device, all to Main.
fn project(plugins: &[&str]) -> (Project, Vec<DeviceId>) {
    let mut p = Project::new("safety");
    let mut devices = Vec::new();
    for (i, plugin) in plugins.iter().enumerate() {
        let tid = p.alloc_track_id();
        let did = p.alloc_device_id();
        let mut t = Track::new(tid, TrackKind::Audio, format!("t{i}"), Color(0));
        t.devices.push(Device::stock(did, *plugin, *plugin));
        p.tracks.push(t);
        devices.push(did);
    }
    (p, devices)
}

fn start(p: &Project, f: Arc<dyn ProcessorFactory>) -> (crate::EngineHandle, AudioProcessor) {
    let (h, proc_) = Engine::new(EngineConfig::headless(SR), f).unwrap();
    h.update_project(p);
    h.flush();
    (h, proc_.unwrap())
}

fn block(proc_: &mut AudioProcessor) -> AudioBuffer {
    let mut buf = AudioBuffer::new(2, BLOCK);
    proc_.process_planar(&mut buf);
    buf
}

#[test]
fn a_panicking_processor_goes_silent_and_the_engine_keeps_running() {
    let (p, devs) = project(&[PANIC, DC]);
    let bombs = Arc::new(Bombs::default());
    let (h, mut proc_) = start(&p, bombs.clone());
    assert!(h.audio_health().is_clean());

    // The block that hits the panic still renders: the healthy track plays,
    // the fallen one is silent, and the fault is on the status.
    for _ in 0..3 {
        block(&mut proc_);
    }
    let health = h.audio_health();
    assert_eq!(health.node_panics, 1, "one device fell, once");
    assert_eq!(health.crashed_devices, vec![devs[0]]);
    assert_eq!(health.callback_panics, 0, "contained at the device, not the callback");
    let out = block(&mut proc_);
    // The DC device alone: 0.25 on Main (the bombed track adds nothing).
    for ch in 0..2 {
        for (i, s) in out.channel(ch).iter().enumerate() {
            assert!((s - 0.25).abs() < 1e-3, "sample {i} ch {ch} = {s}");
        }
    }

    // A fallen device is not called again, not even after a recompile that
    // keeps it (it would panic, and be counted, a second time). The
    // allocation count below measures only the blocks AFTER the fall (the
    // skipped device costs nothing); the normal, guarded path is measured in
    // `the_guarded_path_does_not_allocate`.
    h.update_project(&p);
    h.flush();
    let mut buf = AudioBuffer::new(2, BLOCK);
    proc_.process_planar(&mut buf);
    let ((), allocs) = count_allocs(|| {
        for _ in 0..200 {
            proc_.process_planar(&mut buf);
        }
    });
    assert_eq!(allocs, 0, "blocks after the fall: a fallen device costs nothing on the audio thread");
    assert_eq!(h.audio_health().node_panics, 1);
    assert_eq!(h.audio_health().crashed_devices, vec![devs[0]]);
}

#[test]
fn a_panic_in_latency_or_after_process_is_contained_too() {
    let (p, devs) = project(&[PANIC_IN_LATENCY, DC]);
    let (h, mut proc_) = start(&p, Arc::new(Bombs::default()));
    for _ in 0..3 {
        block(&mut proc_);
    }
    assert_eq!(h.audio_health().crashed_devices, vec![devs[0]]);
    let out = block(&mut proc_);
    assert!((out.channel(0)[BLOCK - 1] - 0.25).abs() < 1e-3, "the other track keeps playing: {}", out.channel(0)[BLOCK - 1]);
}

#[test]
fn the_guarded_path_does_not_allocate() {
    // Healthy devices, no panic anywhere: the catch_unwind around every
    // device and around the whole callback costs no heap operation.
    let (p, _) = project(&[DC, DC, NAN]);
    let (h, mut proc_) = start(&p, Arc::new(Bombs::default()));
    let mut data = vec![0.0f32; 2 * BLOCK];
    for _ in 0..4 {
        assert!(proc_.process_interleaved_guarded(&mut data, 2, 0));
    }
    let ((), allocs) = count_allocs(|| {
        for _ in 0..300 {
            assert!(proc_.process_interleaved_guarded(&mut data, 2, 0));
        }
    });
    assert_eq!(allocs, 0, "guarded callbacks and nodes, normal path");
    assert!(h.audio_health().sanitized_samples > 0, "(the NaN device runs: the repair path is in the count too)");
}

#[test]
fn a_panic_in_a_callback_silences_the_engine_until_a_new_graph_arrives() {
    let (p, _) = project(&[DC]);
    let (h, mut proc_) = start(&p, Arc::new(Bombs::default()));
    let mut data = vec![0.5f32; 2 * 300];
    assert!(proc_.process_interleaved_guarded(&mut data, 2, 0));
    assert!((data[2 * 299] - 0.25).abs() < 1e-3);
    assert_eq!(h.audio_health().callback_panics, 0);

    // Every block panics from now on, until the hook is lowered.
    proc_.fail_blocks = 1_000;
    data.fill(0.5);
    assert!(!proc_.process_interleaved_guarded(&mut data, 2, 0), "reports the fault");
    assert!(data.iter().all(|s| *s == 0.0), "the whole buffer is silence");
    assert_eq!(h.audio_health().callback_panics, 1);
    assert!(!h.audio_health().is_clean());
    assert!(h.audio_health().last_panic.as_deref().is_some_and(|w| w.contains("rt.rs")), "{:?}", h.audio_health().last_panic);

    // No retry: the callback stays silent without entering the block again
    // (it would panic again and count again).
    for _ in 0..10 {
        data.fill(0.5);
        assert!(!proc_.process_interleaved_guarded(&mut data, 2, 0));
        assert!(data.iter().all(|s| *s == 0.0));
    }
    assert_eq!(h.audio_health().callback_panics, 1, "not retried");
    assert_eq!(proc_.fail_blocks, 999, "the block was not entered again");

    // A new graph arrives (the control thread also asks for one on its own
    // after a callback panic): one more attempt, which fails again here...
    // (A real change: an identical project is absorbed without a compile.)
    let mut p = p;
    p.tracks[1].mute = true;
    h.update_project(&p);
    h.flush();
    assert!(!proc_.process_interleaved_guarded(&mut data, 2, 0));
    assert_eq!(h.audio_health().callback_panics, 2);
    assert!(!proc_.process_interleaved_guarded(&mut data, 2, 0), "and halts again");
    assert_eq!(h.audio_health().callback_panics, 2);

    // ...and with the fault gone, the next graph brings the sound back.
    proc_.fail_blocks = 0;
    p.tracks[1].mute = false;
    h.update_project(&p);
    h.flush();
    assert!(proc_.process_interleaved_guarded(&mut data, 2, 0));
    assert!((data[2 * 299] - 0.25).abs() < 1e-3, "{}", data[2 * 299]);
    assert!(proc_.process_interleaved_guarded(&mut data, 2, 0));
    assert_eq!(h.audio_health().callback_panics, 2);
}

#[test]
fn the_panic_hook_is_silent_on_the_audio_thread_and_delegates_elsewhere() {
    use std::sync::atomic::AtomicUsize;
    static OTHER: AtomicUsize = AtomicUsize::new(0);
    let (p, _) = project(&[DC]);
    let (_h, mut proc_) = start(&p, Arc::new(Bombs::default()));
    // The constructor installed our hook over the original one. Swap it for
    // a fresh wrapper over a counter (standing for "the previous hook") and
    // restore it at the end: panics off the audio thread must reach the
    // previous hook; panics on it must not.
    let ours = std::panic::take_hook();
    let test_thread = std::thread::current().id();
    std::panic::set_hook(crate::rt::audio_aware_hook(Box::new(move |_| {
        if std::thread::current().id() == test_thread {
            OTHER.fetch_add(1, Ordering::Relaxed);
        }
    })));
    let before = OTHER.load(Ordering::Relaxed);
    let _ = std::panic::catch_unwind(|| panic!("elsewhere"));
    assert_eq!(OTHER.load(Ordering::Relaxed), before + 1, "off the audio thread the previous hook runs");
    proc_.fail_blocks = 1;
    let mut data = vec![0.0f32; 2 * 64];
    assert!(!proc_.process_interleaved_guarded(&mut data, 2, 0));
    assert_eq!(OTHER.load(Ordering::Relaxed), before + 1, "on the audio thread nothing is printed or delegated");
    std::panic::set_hook(ours);
}

#[test]
fn the_output_bus_never_carries_a_non_finite_sample() {
    let (p, _) = project(&[NAN]);
    let (h, mut proc_) = start(&p, Arc::new(Bombs::default()));
    assert_eq!(h.audio_health().sanitized_samples, 0);
    let mut seen = 0usize;
    for _ in 0..4 {
        let out = block(&mut proc_);
        for ch in 0..2 {
            assert!(out.channel(ch).iter().all(|s| s.is_finite()), "block output must be finite");
        }
        seen += 1;
    }
    // 4 bad samples per block, four blocks.
    assert_eq!(h.audio_health().sanitized_samples, 4 * seen as u64);
    // The repaired samples are zero, the rest of the signal is untouched.
    let out = block(&mut proc_);
    assert_eq!(out.channel(0)[3], 0.0);
    assert_eq!(out.channel(0)[10], 0.0);
    assert_eq!(out.channel(1)[7], 0.0);
    assert_eq!(out.channel(1)[8], 0.0);
    assert!((out.channel(0)[4] - 0.25).abs() < 1e-3);
    // The meters saw the repaired signal.
    let m = h.master_meter();
    assert!(m.peak.iter().all(|v| v.is_finite()) && m.rms.iter().all(|v| v.is_finite()));

    // The interleaved device path too.
    let mut data = vec![0.0f32; 2 * BLOCK];
    proc_.process_interleaved(&mut data, 2);
    assert!(data.iter().all(|s| s.is_finite()));
    assert_eq!(h.audio_health().sanitized_samples, 4 * (seen as u64 + 2));
}

// ───────────────────────────── 0.2 · Compiles that degrade ─────────────────────────────

fn compile_plain(p: &Project) -> Result<crate::CompiledGraph, crate::CompileError> {
    let mut pool = ProcessorPool::new();
    let samples = SampleCache::blocking();
    compile(
        p,
        &mut CompileContext {
            factory: &Bombs::default(),
            pool: &mut pool,
            samples: &samples,
            sample_rate: SR,
            max_frames: BLOCK,
            prev_meters: None,
            taps: Vec::new(),
            analysis: None,
        },
    )
}

/// What the Main output holds in the middle of a short render.
fn rendered_level(p: &Project) -> f32 {
    let out = render_offline(p, &Bombs::default(), (0, PPQ), SR, BLOCK).unwrap();
    out.channel(0)[4000]
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

#[test]
fn an_invalid_output_goes_to_main_with_a_warning() {
    type MakeRoute = fn(&mut Project, TrackId) -> Output;
    let cases: [(&str, MakeRoute); 3] = [
        ("unknown track", |_, _| Output::Track(TrackId(9_999))),
        ("itself", |_, own| Output::Track(own)),
        ("a folder", |p, _| Output::Track(add(p, TrackKind::Folder, "folder", None))),
    ];
    for (what, make) in cases {
        let mut p = Project::new("route");
        let x = add(&mut p, TrackKind::Audio, "lonely", Some(DC));
        let out = make(&mut p, x);
        p.track_mut(x).unwrap().output = out;
        let g = compile_plain(&p).unwrap_or_else(|e| panic!("{what}: the compile must degrade, not fail: {e}"));
        let w = g.warnings();
        assert_eq!(w.len(), 1, "{what}: {w:?}");
        assert!(w[0].contains("lonely") && w[0].contains("Main"), "{what}: {w:?}");
        let i = g.track_index(x).unwrap();
        assert_eq!(g.tracks[i].output, crate::graph::OutputTarget::Track(g.track_index(p.main_track().id).unwrap()));
        assert!((rendered_level(&p) - 0.25).abs() < 1e-3, "{what}: the track is heard on Main");
    }
}

#[test]
fn an_output_cycle_is_cut_where_it_closes_and_the_rest_plays() {
    let mut p = Project::new("cycle");
    let x = add(&mut p, TrackKind::Audio, "x", Some(DC));
    let y = add(&mut p, TrackKind::Audio, "y", Some(DC));
    let z = add(&mut p, TrackKind::Audio, "z", Some(DC));
    p.track_mut(x).unwrap().output = Output::Track(y);
    p.track_mut(y).unwrap().output = Output::Track(x);
    let g = compile_plain(&p).expect("a cycle must not fail the compile");
    assert_eq!(g.warnings().len(), 1, "{:?}", g.warnings());
    let w = &g.warnings()[0];
    assert!(w.contains("cycle") && w.contains('y') && w.contains('x'), "{w}");
    // y (the track whose output closed the loop) goes nowhere; x still feeds y.
    assert_eq!(g.tracks[g.track_index(y).unwrap()].output, crate::graph::OutputTarget::None);
    assert!(matches!(g.tracks[g.track_index(x).unwrap()].output, crate::graph::OutputTarget::Track(_)));
    // The independent track still sounds: Main = z alone.
    assert!((rendered_level(&p) - 0.25).abs() < 1e-3);
    let _ = z;
}

#[test]
fn a_send_cycle_drops_the_send_that_closes_it() {
    let mut p = Project::new("sends");
    let x = add(&mut p, TrackKind::Audio, "x", Some(DC));
    let r = add(&mut p, TrackKind::Return, "r", None);
    p.track_mut(x).unwrap().sends.push(Send { target: r, level_db: 0.0, pre_fader: false });
    p.track_mut(r).unwrap().sends.push(Send { target: x, level_db: 0.0, pre_fader: false });
    let g = compile_plain(&p).expect("a send loop must not fail the compile");
    assert_eq!(g.warnings().len(), 1, "{:?}", g.warnings());
    assert!(g.warnings()[0].contains("cycle") && g.warnings()[0].contains("send"), "{:?}", g.warnings());
    assert_eq!(g.tracks[g.track_index(x).unwrap()].sends.len(), 1, "x keeps its send to the return");
    assert_eq!(g.tracks[g.track_index(r).unwrap()].sends.len(), 0, "the return's send back is the one cut");
    // Direct 0.25 plus the send's 0.25 through the return.
    assert!((rendered_level(&p) - 0.5).abs() < 1e-3);
}

#[test]
fn the_engine_exposes_the_warnings_of_a_degraded_compile() {
    let mut p = Project::new("api");
    let x = add(&mut p, TrackKind::Audio, "x", Some(DC));
    let y = add(&mut p, TrackKind::Audio, "y", Some(DC));
    p.track_mut(x).unwrap().output = Output::Track(y);
    p.track_mut(y).unwrap().output = Output::Track(x);
    let (h, mut proc_) = start(&p, Arc::new(Bombs::default()));
    assert!(h.compile_generation() > 0, "the first compile bumped the generation");
    assert_eq!(h.last_error(), None);
    assert_eq!(h.warnings().len(), 1, "{:?}", h.warnings());
    block(&mut proc_);
    assert!(proc_.graph_mut().is_some(), "the graph was installed and plays");

    // Fixing the project clears the warning and bumps the generation again.
    let before = h.compile_generation();
    p.track_mut(y).unwrap().output = Output::Main;
    h.update_project(&p);
    h.flush();
    assert!(h.compile_generation() > before);
    assert!(h.warnings().is_empty());
}

// ───────────────────────────── 0.6 · Faders start at their targets ─────────────────────────────

fn clip_project(frames: Vec<f32>, send: bool) -> (Project, SampleCache) {
    let mut p = Project::new("prime");
    let t = add(&mut p, TrackKind::Audio, "audio", None);
    let ret = add(&mut p, TrackKind::Return, "ret", None);
    if send {
        p.track_mut(t).unwrap().sends.push(Send { target: ret, level_db: 0.0, pre_fader: false });
    }
    let path = std::path::PathBuf::from("mem://prime.wav");
    let samples = SampleCache::blocking();
    let n = frames.len() as u64;
    samples.insert(path.clone(), Arc::new(DecodedAudio::from_planar(vec![frames], 48_000)));
    let sid = p.alloc_source_id();
    p.sources.insert(sid, Source::Audio(AudioSource { path, hash: None, sample_rate: 48_000, channels: 1, frames: n, warp: Warp::default(), key: None }));
    let cid = p.alloc_clip_id();
    p.clips.insert(cid, Clip::new("c", Color(0), sid, 4 * PPQ));
    let pid = p.alloc_placement_id();
    p.track_mut(t).unwrap().arrangement.push(Placement { id: pid, clip: cid, start: 0, lane: 0 });
    (p, samples)
}

#[test]
fn an_offline_render_starts_at_full_gain_on_sample_zero() {
    // A click on sample 0 and a decaying ramp after it.
    let mut data: Vec<f32> = (0..4096).map(|i| 0.8 - 0.5 * (i as f32 / 4096.0)).collect();
    data[0] = 1.0;
    for send in [false, true] {
        let (p, samples) = clip_project(data.clone(), send);
        let out = render_offline_with(&p, &FallbackFactory, (0, 2 * PPQ), SR, BLOCK, samples).unwrap();
        let ch = out.channel(0);
        let gain = if send { 2.0 } else { 1.0 };
        for k in [0usize, 1, 2, 15, 31, 32, 100] {
            let want = data[k] * gain;
            assert!((ch[k] - want).abs() < 1e-5, "send={send} sample {k}: {} vs {want}", ch[k]);
        }
    }
}

#[test]
fn an_adopted_fader_keeps_its_ramp_instead_of_snapping() {
    let (mut p, _) = project(&[DC]);
    let (h, mut proc_) = start(&p, Arc::new(Bombs::default()));
    for _ in 0..3 {
        block(&mut proc_);
    }
    // -20 dB on the track: a track that already plays glides there.
    p.tracks[1].volume_db = -20.0;
    h.update_project(&p);
    h.flush();
    let out = block(&mut proc_);
    assert!(out.channel(0)[0] > 0.2, "starts from the old gain: {}", out.channel(0)[0]);
    assert!((out.channel(0)[BLOCK - 1] - 0.025).abs() < 1e-3, "ends at the new one: {}", out.channel(0)[BLOCK - 1]);
}

// ───────────────────────────── 0.12 · Automation removed ─────────────────────────────

fn gain_param(proc_: &mut AudioProcessor, device: DeviceId) -> f32 {
    let g = proc_.graph_mut().expect("graph");
    for t in &mut g.tracks {
        if let Some(p) = find_proc_mut(&mut t.devices, device) {
            // SAFETY: the test drives the processor on this thread only.
            return unsafe { p.proc.get_mut() }.get_param(0);
        }
    }
    panic!("device {device} not in the graph");
}

fn gain_project(knob: Option<f32>) -> (Project, DeviceId) {
    let mut p = Project::new("auto");
    let t = add(&mut p, TrackKind::Audio, "fx", Some(TEST_GAIN));
    let d = p.track(t).unwrap().devices[0].id;
    if let Some(v) = knob {
        p.track_mut(t).unwrap().devices[0].params.insert(0, v);
    }
    let mut lane = AutomationLane::new(AutomationTarget::DeviceParam { device: d, param: 0 });
    lane.points = vec![AutomationPoint { tick: 0, value: 0.9, curve: 0.0 }, AutomationPoint { tick: PPQ, value: 0.9, curve: 0.0 }];
    p.track_mut(t).unwrap().automation.push(lane);
    (p, d)
}

#[test]
fn removing_the_automation_returns_the_processor_to_the_knob() {
    for removal in ["delete the lane", "disable the lane"] {
        for (knob, back) in [(Some(0.25f32), 0.25f32), (None, 0.5)] {
            let (mut p, d) = gain_project(knob);
            let (h, mut proc_) = Engine::new(EngineConfig::headless(SR), Arc::new(FallbackFactory)).unwrap();
            let mut proc_ = proc_.take().unwrap();
            h.update_project(&p);
            h.flush();
            for _ in 0..2 {
                block(&mut proc_);
            }
            assert!((gain_param(&mut proc_, d) - 0.9).abs() < 1e-6, "{removal}: the lane drives the processor");

            let t = p.tracks[1].id;
            match removal {
                "delete the lane" => p.track_mut(t).unwrap().automation.clear(),
                _ => p.track_mut(t).unwrap().automation[0].enabled = false,
            }
            h.update_project(&p);
            h.flush();
            block(&mut proc_);
            let v = gain_param(&mut proc_, d);
            assert!((v - back).abs() < 1e-6, "{removal}, knob {knob:?}: back to {back}, got {v}");

            // And it stays at the knob on later recompiles.
            h.update_project(&p);
            h.flush();
            block(&mut proc_);
            assert!((gain_param(&mut proc_, d) - back).abs() < 1e-6);
        }
    }
}

// ───────────────────────────── Panics outside `process` ─────────────────────────────

#[test]
fn a_panic_in_set_param_at_install_marks_the_device_and_the_graph_still_installs() {
    let (mut p, devs) = project(&[PANIC_IN_SET_PARAM, DC]);
    p.tracks[1].devices[0].params.insert(0, 0.3);
    let bombs = Arc::new(Bombs::default());
    let (h, mut proc_) = start(&p, bombs.clone());
    for _ in 0..2 {
        block(&mut proc_);
    }
    assert!(h.audio_health().is_clean());

    // From now on `set_param` panics. A changed knob makes the new graph
    // carry an init param for it, applied on the audio thread at install;
    // the other track changes too, to show the new graph is the one running.
    bombs.armed.store(true, Ordering::Relaxed);
    p.tracks[1].devices[0].params.insert(0, 0.7);
    p.tracks[2].volume_db = -20.0;
    h.update_project(&p);
    h.flush();
    for _ in 0..3 {
        block(&mut proc_);
    }
    {
        let g = proc_.graph_mut().expect("the graph survived its own install");
        assert_eq!(g.tracks[g.track_index(p.tracks[2].id).unwrap()].volume_db, -20.0, "the new graph is installed");
    }
    let health = h.audio_health();
    assert_eq!(health.callback_panics, 0);
    assert_eq!(health.node_panics, 1);
    assert_eq!(health.crashed_devices, vec![devs[0]]);
    let out = block(&mut proc_);
    assert!((out.channel(0)[BLOCK - 1] - 0.025).abs() < 1e-3, "the rest plays: {}", out.channel(0)[BLOCK - 1]);

    // A fallen device is not called again by later installs.
    p.tracks[1].devices[0].params.insert(0, 0.9);
    h.update_project(&p);
    h.flush();
    block(&mut proc_);
    assert_eq!(h.audio_health().node_panics, 1);
}

// ───────────────────────────── Graphs that replace graphs ─────────────────────────────

fn playing_dc() -> (Project, crate::EngineHandle, AudioProcessor) {
    let mut p = Project::new("swap");
    add(&mut p, TrackKind::Audio, "a", Some(DC));
    add(&mut p, TrackKind::Return, "r1", None);
    add(&mut p, TrackKind::Return, "r2", None);
    let (h, mut proc_) = start(&p, Arc::new(Bombs::default()));
    for _ in 0..3 {
        block(&mut proc_);
    }
    (p, h, proc_)
}

#[test]
fn a_track_added_while_playing_fades_in_from_silence() {
    let (mut p, h, mut proc_) = playing_dc();
    add(&mut p, TrackKind::Audio, "b", Some(DC));
    h.update_project(&p);
    h.flush();
    let out = block(&mut proc_);
    let l = out.channel(0);
    // `a` alone is 0.25; `b` rises over the first 32 samples to its own 0.25.
    assert!(l[0] < 0.27, "no click from the new track: {}", l[0]);
    assert!(l[16] > l[0] && l[16] < l[40], "ramping: {} {} {}", l[0], l[16], l[40]);
    assert!((l[40] - 0.5).abs() < 1e-3, "{}", l[40]);
}

#[test]
fn a_send_added_or_redirected_while_playing_fades_in_from_silence() {
    let (mut p, h, mut proc_) = playing_dc();
    let (a, r1, r2) = (p.tracks[1].id, p.tracks[2].id, p.tracks[3].id);
    // Added.
    p.track_mut(a).unwrap().sends.push(Send { target: r1, level_db: 0.0, pre_fader: false });
    h.update_project(&p);
    h.flush();
    let l = block(&mut proc_).channel(0).to_vec();
    assert!(l[0] < 0.27, "added send, no click: {}", l[0]);
    assert!(l[16] > l[0] && (l[40] - 0.5).abs() < 1e-3, "{} {} {}", l[0], l[16], l[40]);
    // Settled, then redirected to the other return: it enters from silence.
    block(&mut proc_);
    p.track_mut(a).unwrap().sends[0].target = r2;
    h.update_project(&p);
    h.flush();
    let l = block(&mut proc_).channel(0).to_vec();
    assert!(l[0] < 0.27, "redirected send, no click: {}", l[0]);
    assert!((l[40] - 0.5).abs() < 1e-3, "{}", l[40]);
    // An unchanged send keeps its gain through a recompile.
    block(&mut proc_);
    h.update_project(&p);
    h.flush();
    let l = block(&mut proc_).channel(0).to_vec();
    assert!((l[0] - 0.5).abs() < 1e-3, "unchanged send continues: {}", l[0]);
}
