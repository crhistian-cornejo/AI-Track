//! Out-of-process hosting through `aitrack-plugin-host` over shared memory.

mod common;

use std::time::{Duration, Instant};

use aitrack_dsp::{Event, EventKind, ProcessContext, Processor, Transport};
use aitrack_host::sandbox::SandboxedPlugin;
use aitrack_host::{HostedPluginInfo, ParamEditKind, PluginFormat};
use common::{constant_buffer, fixture};

const FRAMES: usize = 256;

fn info(format: PluginFormat) -> HostedPluginInfo {
    let f = fixture();
    match format {
        PluginFormat::Clap => aitrack_host::clap::enumerate(&f.clap).unwrap().remove(0),
        PluginFormat::Vst3 => aitrack_host::vst3::enumerate(&f.vst3).unwrap().remove(0),
    }
}

fn spawn(format: PluginFormat) -> SandboxedPlugin {
    let mut p = SandboxedPlugin::spawn_with(env!("CARGO_BIN_EXE_aitrack-plugin-host").as_ref(), &info(format)).expect("spawn sandbox");
    // Generous budget: debug builds on a loaded CI machine.
    p.set_block_budget(Some(Duration::from_secs(2)));
    p.prepare(48_000.0, FRAMES);
    p
}

fn run(p: &mut SandboxedPlugin, input: f32, events: &[Event]) -> aitrack_dsp::AudioBuffer {
    let mut buf = constant_buffer(2, FRAMES, input);
    let transport = Transport::default();
    let mut ctx = ProcessContext { sample_rate: 48_000.0, buffer: &mut buf, sidechain: None, events, transport: &transport };
    p.process(&mut ctx);
    buf
}

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-6
}

#[test]
fn sandbox_processes_params_events_and_state() {
    for format in [PluginFormat::Clap, PluginFormat::Vst3] {
        let mut p = spawn(format);
        assert_eq!(p.info().name, "AI-Track Test Gain");
        assert_eq!(p.latency(), 32, "{format:?}");

        let out = run(&mut p, 0.5, &[]);
        assert!(out.channel(0).iter().chain(out.channel(1)).all(|&s| close(s, 0.5)), "{format:?} unity gain");

        p.set_param(0, 0.25);
        let out = run(&mut p, 1.0, &[]);
        assert!(out.channel(0).iter().all(|&s| close(s, 0.5)), "{format:?} set_param");

        let events = [Event { offset: 64, kind: EventKind::Param { id: 0, value: 0.0 } }];
        let out = run(&mut p, 1.0, &events);
        assert!(out.channel(1)[..64].iter().all(|&s| close(s, 0.5)));
        assert!(out.channel(1)[64..].iter().all(|&s| close(s, 0.0)), "{format:?} sample-accurate");

        p.set_param(0, 0.75);
        let _ = run(&mut p, 1.0, &[]);
        let state = p.save_state();
        assert!(!state.is_empty());
        let mut q = spawn(format);
        q.load_state(&state);
        assert!(close(q.get_param(0), 0.75), "{format:?} state: {}", q.get_param(0));
        let out = run(&mut q, 1.0, &[]);
        assert!(out.channel(0).iter().all(|&s| close(s, 1.5)));
        assert_eq!(p.late_blocks() + q.late_blocks(), 0);
    }
}

#[test]
fn sandbox_forwards_gui_edits() {
    let mut p = spawn(PluginFormat::Clap);
    let mut edits = p.take_param_edits().unwrap();
    let events = [Event { offset: 0, kind: EventKind::NoteOn { channel: 0, key: 127, velocity: 1.0, note_id: -1 } }];
    let _ = run(&mut p, 1.0, &events);
    let got: Vec<_> = std::iter::from_fn(|| edits.pop().ok()).collect();
    assert_eq!(got.iter().map(|e| e.kind).collect::<Vec<_>>(), vec![ParamEditKind::Begin, ParamEditKind::Value, ParamEditKind::End]);
    assert!(close(p.get_param(0), 0.25));
}

#[test]
fn sandbox_handoff_round_trip_time() {
    let mut p = spawn(PluginFormat::Clap);
    p.set_block_budget(None); // real budget: 70 % of a 256-sample block
    let _ = run(&mut p, 0.0, &[]);
    let n = 500;
    let start = Instant::now();
    for _ in 0..n {
        let _ = run(&mut p, 0.1, &[]);
    }
    let per_block = start.elapsed() / n;
    eprintln!("sandbox round trip: {per_block:?} per 256-frame block, {} late of {n}", p.late_blocks());
    assert!(p.late_blocks() < n as u64 / 10, "too many late blocks: {}", p.late_blocks());
}

#[test]
fn sandbox_reports_missing_plugin() {
    let mut bad = info(PluginFormat::Clap);
    bad.uid = "org.example.missing".into();
    let r = SandboxedPlugin::spawn_with(env!("CARGO_BIN_EXE_aitrack-plugin-host").as_ref(), &bad);
    assert!(r.is_err());
}
