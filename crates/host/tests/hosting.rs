//! In-process hosting of the deterministic `test-gain` fixture, in both
//! formats: processing, parameters (set_param and sample-accurate events),
//! state round-trip, latency, GUI-edit queue and thread confinement.

mod common;

use aitrack_dsp::{Event, EventKind, ProcessContext, Processor, Transport};
use aitrack_host::{HostError, HostedPlugin, ParamEditKind, PluginFormat};
use common::{constant_buffer, fixture};

const FRAMES: usize = 256;

fn load(format: PluginFormat) -> HostedPlugin {
    let f = fixture();
    let path = match format {
        PluginFormat::Clap => &f.clap,
        PluginFormat::Vst3 => &f.vst3,
    };
    let mut p = HostedPlugin::load_path(format, path, None).expect("load fixture");
    p.prepare(48_000.0, FRAMES);
    assert!(p.is_prepared(), "{format:?} fixture failed to activate");
    p
}

fn run(p: &mut HostedPlugin, input: f32, events: &[Event]) -> aitrack_dsp::AudioBuffer {
    let mut buf = constant_buffer(2, FRAMES, input);
    let transport = Transport { playing: true, ..Default::default() };
    let mut ctx = ProcessContext { sample_rate: 48_000.0, buffer: &mut buf, sidechain: None, events, transport: &transport };
    p.process(&mut ctx);
    buf
}

fn assert_all(buf: &aitrack_dsp::AudioBuffer, range: std::ops::Range<usize>, expected: f32) {
    for ch in 0..2 {
        for (i, s) in buf.channel(ch)[range.clone()].iter().enumerate() {
            assert!((s - expected).abs() < 1e-6, "ch {ch} sample {}: {s} != {expected}", range.start + i);
        }
    }
}

fn both(test: impl Fn(PluginFormat)) {
    test(PluginFormat::Clap);
    test(PluginFormat::Vst3);
}

#[test]
fn info_params_latency() {
    both(|format| {
        let p = load(format);
        let info = p.info();
        assert_eq!(info.name, "AI-Track Test Gain", "{format:?}");
        assert_eq!(info.vendor, "AI-Track");
        assert_eq!(info.params.len(), 1);
        assert_eq!(info.params[0].name, "Gain");
        assert_eq!(info.inputs, 2);
        assert_eq!(info.outputs, 2);
        assert!(!p.hosted_info().is_instrument);
        assert_eq!(p.latency(), 32, "{format:?} latency");
        // Default gain 1.0 → normalised 0.5 in both formats.
        assert!((p.get_param(0) - 0.5).abs() < 1e-6, "{format:?} default");
    });
}

#[test]
fn process_applies_gain() {
    both(|format| {
        let mut p = load(format);
        let out = run(&mut p, 0.5, &[]);
        assert_all(&out, 0..FRAMES, 0.5);
        // Normalised 0.25 → gain 0.5.
        p.set_param(0, 0.25);
        let out = run(&mut p, 0.5, &[]);
        assert_all(&out, 0..FRAMES, 0.25);
        assert!((p.get_param(0) - 0.25).abs() < 1e-6);
    });
}

#[test]
fn param_events_are_sample_accurate() {
    both(|format| {
        let mut p = load(format);
        let events = [Event { offset: 100, kind: EventKind::Param { id: 0, value: 0.0 } }];
        let out = run(&mut p, 1.0, &events);
        assert_all(&out, 0..100, 1.0);
        assert_all(&out, 100..FRAMES, 0.0);
        assert_eq!(p.get_param(0), 0.0);
    });
}

#[test]
fn state_round_trip() {
    both(|format| {
        let mut a = load(format);
        a.set_param(0, 0.75);
        let _ = run(&mut a, 1.0, &[]);
        let state = a.save_state();
        assert!(!state.is_empty(), "{format:?} produced no state");

        let mut b = load(format);
        assert!((b.get_param(0) - 0.5).abs() < 1e-6);
        b.load_state(&state);
        assert!((b.get_param(0) - 0.75).abs() < 1e-6, "{format:?} param after load: {}", b.get_param(0));
        let out = run(&mut b, 1.0, &[]);
        assert_all(&out, 0..FRAMES, 1.5);
    });
}

#[test]
fn clap_gui_edits_reach_the_daw() {
    let mut p = load(PluginFormat::Clap);
    let mut edits = p.take_param_edits().expect("edit queue");
    assert!(p.take_param_edits().is_none(), "queue can only be taken once");
    // Key 127 makes the fixture emit gesture-begin, value 0.5 (plain), gesture-end.
    let events = [Event { offset: 0, kind: EventKind::NoteOn { channel: 0, key: 127, velocity: 1.0, note_id: -1 } }];
    let _ = run(&mut p, 1.0, &events);
    let got: Vec<_> = std::iter::from_fn(|| edits.pop().ok()).collect();
    assert_eq!(got.len(), 3, "{got:?}");
    assert_eq!(got[0].kind, ParamEditKind::Begin);
    assert_eq!(got[1].kind, ParamEditKind::Value);
    assert!((got[1].value - 0.25).abs() < 1e-6);
    assert_eq!(got[2].kind, ParamEditKind::End);
    assert!((p.get_param(0) - 0.25).abs() < 1e-6);
}

#[test]
fn param_text_and_idle() {
    both(|format| {
        let p = load(format);
        let c = p.controller();
        assert_eq!(c.param_text(0, 0.5).as_deref(), Some("1.00x"), "{format:?}");
        c.idle();
        assert!(!c.is_crashed());
    });
}

#[test]
fn main_thread_calls_are_confined() {
    both(|format| {
        let p = load(format);
        let c = p.controller();
        let r = std::thread::spawn(move || c.save_state().map(|_| ())).join().unwrap();
        assert!(matches!(r, Err(HostError::WrongThread)), "{format:?}: {r:?}");
    });
}

#[test]
fn reprepare_and_reset() {
    both(|format| {
        let mut p = load(format);
        p.set_param(0, 0.25);
        p.prepare(44_100.0, 512);
        assert!(p.is_prepared());
        p.reset();
        let out = run(&mut p, 1.0, &[]);
        assert_all(&out, 0..FRAMES, 0.5);
    });
}

#[test]
fn enumerate_both_formats() {
    let f = fixture();
    let clap = aitrack_host::clap::enumerate(&f.clap).unwrap();
    assert_eq!(clap.len(), 1);
    assert_eq!(clap[0].uid, "org.aitrack.test-gain");
    assert_eq!(clap[0].category, "audio-effect|stereo");
    let vst3 = aitrack_host::vst3::probe(&f.vst3).unwrap();
    assert_eq!(vst3.len(), 1);
    assert_eq!(vst3[0].category, "Fx|Tools");
    assert_eq!(vst3[0].version, "1.0.0");
    assert!(vst3[0].probed);
    assert_eq!(vst3[0].params, 1);
    assert_eq!(vst3[0].uid.len(), 32);
}
