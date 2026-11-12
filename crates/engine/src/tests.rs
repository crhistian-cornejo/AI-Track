//! Engine integration tests: golden offline renders, routing, PDC, session,
//! export formats and the zero-allocation guarantee of the audio thread.

use crate::alloc_counter::count_allocs;
use crate::automation::volume_normalized_from_db;
use crate::compile::{compile, CompileContext, CompileError};
use crate::export::*;
use crate::fallback::{FallbackFactory, TEST_GAIN, TEST_LATENCY, TEST_SYNTH};
use crate::graph::DeviceNode;
use crate::sample_cache::{DecodedAudio, SampleCache};
use crate::shared::ProcessorPool;
use crate::status::SlotState;
use crate::{Engine, EngineConfig, MidiMessage};
use aitrack_dsp::AudioBuffer;
use aitrack_model::time::TempoPoint;
use aitrack_model::*;
use std::sync::Arc;

const SR: f64 = 48_000.0;
/// Samples per beat at 120 BPM and 48 kHz.
const BEAT: usize = 24_000;

// ───────────────────────────── Builders ─────────────────────────────

struct P {
    p: Project,
}

impl P {
    fn new(bpm: f64) -> Self {
        let mut p = Project::new("test");
        p.tempo = TempoMap::constant(bpm);
        Self { p }
    }

    fn main(&self) -> TrackId {
        self.p.main_track().id
    }

    fn track(&mut self, kind: TrackKind, name: &str) -> TrackId {
        let id = self.p.alloc_track_id();
        self.p.tracks.push(Track::new(id, kind, name, Color(0)));
        id
    }

    fn t(&mut self, id: TrackId) -> &mut Track {
        self.p.track_mut(id).unwrap()
    }

    fn device(&mut self, track: TrackId, plugin: &str, params: &[(u32, f32)], state: Option<Vec<u8>>) -> DeviceId {
        let id = self.p.alloc_device_id();
        let mut d = Device::stock(id, plugin, plugin);
        for &(k, v) in params {
            d.params.insert(k, v);
        }
        d.state = state;
        self.t(track).devices.push(d);
        id
    }

    /// Instrument track with a saw synth (non-zero from the first sample of a note).
    fn synth_track(&mut self, name: &str) -> (TrackId, DeviceId) {
        let t = self.track(TrackKind::Instrument, name);
        let d = self.device(t, TEST_SYNTH, &[(0, 1.0), (5, 0.5)], None);
        (t, d)
    }

    fn notes(&mut self, notes: &[(u8, Tick, Tick)], length: Tick, looping: Option<(Tick, Tick)>) -> ClipId {
        let sid = self.p.alloc_source_id();
        let mut src = NoteSource::default();
        for &(key, start, len) in notes {
            let nid = self.p.alloc_note_id();
            src.notes.push(Note::new(nid, key, start, len, 1.0));
        }
        self.p.sources.insert(sid, Source::Notes(src));
        let cid = self.p.alloc_clip_id();
        self.p.clips.insert(
            cid,
            Clip { looping: looping.map(|(start, end)| LoopRegion { start, end }), ..Clip::new("clip", Color(0), sid, length) },
        );
        cid
    }

    fn place(&mut self, track: TrackId, clip: ClipId, start: Tick) {
        let id = self.p.alloc_placement_id();
        self.t(track).arrangement.push(Placement { id, clip, start, lane: 0 });
    }
}

fn render(p: &Project, ticks: (Tick, Tick)) -> AudioBuffer {
    render_offline(p, &FallbackFactory, ticks, SR, 256).unwrap()
}

fn first_nonzero(buf: &AudioBuffer, from: usize) -> Option<usize> {
    buf.channel(0)[from..].iter().position(|s| s.abs() > 1e-6).map(|i| i + from)
}

/// Starts of non-silent runs (after at least `gap` silent samples).
fn onsets(data: &[f32], gap: usize) -> Vec<usize> {
    let mut out = Vec::new();
    let mut silent = gap;
    for (i, s) in data.iter().enumerate() {
        if s.abs() > 1e-6 {
            if silent >= gap {
                out.push(i);
            }
            silent = 0;
        } else {
            silent += 1;
        }
    }
    out
}

fn peak(data: &[f32]) -> f32 {
    data.iter().fold(0.0f32, |m, s| m.max(s.abs()))
}

fn compile_plain(p: &Project) -> Result<crate::CompiledGraph, CompileError> {
    let mut pool = ProcessorPool::new();
    let samples = SampleCache::blocking();
    compile(
        p,
        &mut CompileContext {
            factory: &FallbackFactory,
            pool: &mut pool,
            samples: &samples,
            sample_rate: SR,
            max_frames: 256,
            prev_meters: None,
            taps: Vec::new(),
            analysis: None,
        },
    )
}

// ───────────────────────────── Golden renders ─────────────────────────────

#[test]
fn note_sounds_exactly_at_its_sample() {
    let mut b = P::new(120.0);
    let (t, _) = b.synth_track("synth");
    let c = b.notes(&[(60, 2 * PPQ, PPQ)], 4 * PPQ, None);
    b.place(t, c, 0);
    let out = render(&b.p, (0, 4 * PPQ));
    assert_eq!(out.frames(), 4 * BEAT);
    assert_eq!(first_nonzero(&out, 0), Some(2 * BEAT));
    // Released (zero release time) exactly at the note end.
    assert!(peak(&out.channel(0)[3 * BEAT..]) < 1e-6);
    assert!(peak(&out.channel(0)[2 * BEAT..3 * BEAT]) > 0.1);
}

#[test]
fn clip_placement_offset_and_render_range() {
    let mut b = P::new(120.0);
    let (t, _) = b.synth_track("synth");
    let c = b.notes(&[(60, 0, PPQ / 2)], PPQ, None);
    b.place(t, c, 3 * PPQ);
    // Render starting at beat 2: the note at beat 3 lands one beat in.
    let out = render(&b.p, (2 * PPQ, 6 * PPQ));
    assert_eq!(first_nonzero(&out, 0), Some(BEAT));
}

#[test]
fn tempo_ramp_timing_matches_tempo_map() {
    let mut b = P::new(60.0);
    b.p.tempo.set_point(TempoPoint { start: 0, bpm: 60.0, ramp: true });
    b.p.tempo.set_point(TempoPoint { start: 4 * PPQ, bpm: 180.0, ramp: false });
    let (t, _) = b.synth_track("synth");
    let c = b.notes(&[(60, 2 * PPQ, PPQ / 4), (62, 3 * PPQ, PPQ / 4)], 4 * PPQ, None);
    b.place(t, c, 0);
    let out = render(&b.p, (0, 4 * PPQ));
    let expect = |tick: Tick| (b.p.tempo.tick_to_samples(tick, SR) - 1e-6).ceil() as usize;
    let on = onsets(out.channel(0), 100);
    assert_eq!(on, vec![expect(2 * PPQ), expect(3 * PPQ)]);
    // Faster than constant 60 BPM (which would be 96000 for beat 2).
    assert!(on[0] < 96_000);
}

#[test]
fn looping_clip_repeats_in_arrangement() {
    let mut b = P::new(120.0);
    let (t, _) = b.synth_track("synth");
    // 1-beat loop with a note at its start, clip 3 beats long.
    let c = b.notes(&[(60, 0, PPQ / 4)], 3 * PPQ, Some((0, PPQ)));
    b.place(t, c, 0);
    let out = render(&b.p, (0, 4 * PPQ));
    assert_eq!(onsets(out.channel(0), 100), vec![0, BEAT, 2 * BEAT]);
}

#[test]
fn transport_loop_wraps_sample_exactly() {
    let mut b = P::new(120.0);
    let (t, _) = b.synth_track("synth");
    let c = b.notes(&[(60, PPQ, PPQ / 4)], 4 * PPQ, None);
    b.place(t, c, 0);
    let (h, proc_) = Engine::new(EngineConfig::headless(SR), Arc::new(FallbackFactory)).unwrap();
    let mut proc_ = proc_.unwrap();
    h.update_project(&b.p);
    h.flush();
    h.set_loop(Some((0, 2 * PPQ)));
    h.play();
    let mut all = Vec::new();
    let mut buf = AudioBuffer::new(2, 333);
    while all.len() < 5 * BEAT {
        proc_.process_planar(&mut buf);
        all.extend_from_slice(buf.channel(0));
    }
    // Note at beat 1 of a 2-beat loop: beats 1, 3, 5 of the timeline.
    assert_eq!(onsets(&all[..5 * BEAT], 100), vec![BEAT, 3 * BEAT]);
    let pos = proc_.position_samples();
    assert!(pos >= 0 && pos < 2 * BEAT as i64, "position {pos} stays inside the loop");
}

#[test]
fn volume_automation_ramp_is_applied() {
    let mut b = P::new(120.0);
    let (t, _) = b.synth_track("synth");
    let c = b.notes(&[(60, 0, 8 * PPQ)], 8 * PPQ, None);
    b.place(t, c, 0);
    let mut lane = AutomationLane::new(AutomationTarget::Volume);
    lane.insert(AutomationPoint { tick: 0, value: 0.0, curve: 0.0 });
    lane.insert(AutomationPoint { tick: 4 * PPQ, value: volume_normalized_from_db(0.0), curve: 0.0 });
    b.t(t).automation.push(lane);
    let out = render(&b.p, (0, 4 * PPQ));
    let d = out.channel(0);
    let reference = render(&{
        let mut q = b.p.clone();
        q.track_mut(t).unwrap().automation.clear();
        q
    }, (0, 4 * PPQ));
    let full = peak(&reference.channel(0)[3 * BEAT..4 * BEAT]);
    // Early part almost silent, late part near full, monotonic in between.
    assert!(peak(&d[..BEAT / 4]) < full * 0.01);
    let quarters: Vec<f32> = (0..4).map(|q| peak(&d[q * BEAT..(q + 1) * BEAT])).collect();
    assert!(quarters.windows(2).all(|w| w[1] > w[0]), "{quarters:?}");
    // Halfway the lane is at about −35 dB.
    let mid = peak(&d[2 * BEAT - 600..2 * BEAT + 600]) / full;
    assert!((0.012..0.026).contains(&mid), "mid gain {mid}");
}

#[test]
fn device_param_automation_reaches_the_processor() {
    let mut b = P::new(120.0);
    let (t, _) = b.synth_track("synth");
    let gain = b.device(t, TEST_GAIN, &[(0, 0.5)], None);
    let c = b.notes(&[(60, 0, 4 * PPQ)], 4 * PPQ, None);
    b.place(t, c, 0);
    let mut lane = AutomationLane::new(AutomationTarget::DeviceParam { device: gain, param: 0 });
    lane.insert(AutomationPoint { tick: 0, value: 0.5, curve: 0.0 });
    lane.insert(AutomationPoint { tick: 2 * PPQ, value: 0.5, curve: 0.0 });
    lane.insert(AutomationPoint { tick: 2 * PPQ + 1, value: 0.0, curve: 0.0 });
    b.t(t).automation.push(lane);
    let out = render(&b.p, (0, 4 * PPQ));
    assert!(peak(&out.channel(0)[..2 * BEAT - 64]) > 0.1);
    // Silent within one control block (32 samples) of the step.
    assert!(peak(&out.channel(0)[2 * BEAT + 64..]) < 1e-6);
}

// ───────────────────────────── PDC ─────────────────────────────

#[test]
fn pdc_aligns_paths_and_stems() {
    let mut b = P::new(120.0);
    let (a, _) = b.synth_track("late");
    b.device(a, TEST_LATENCY, &[], Some(1000u32.to_le_bytes().to_vec()));
    let (bb, _) = b.synth_track("direct");
    let c = b.notes(&[(60, 2 * PPQ, PPQ)], 4 * PPQ, None);
    b.place(a, c, 0);
    b.place(bb, c, 0);

    let g = compile_plain(&b.p).unwrap();
    assert_eq!(g.total_latency(), 1000);

    let mut single = b.p.clone();
    single.tracks.retain(|t| t.id != a);
    let one = render(&single, (0, 4 * PPQ));
    let both = render(&b.p, (0, 4 * PPQ));
    assert_eq!(first_nonzero(&both, 0), Some(2 * BEAT));
    for k in [2 * BEAT, 2 * BEAT + 500, 2 * BEAT + 1500] {
        let (x, y) = (both.channel(0)[k], one.channel(0)[k]);
        assert!((x - 2.0 * y).abs() < 1e-4, "sample {k}: {x} vs 2×{y}");
    }

    // Stems of both tracks start on the same sample.
    let opts = RenderOptions { range: RenderRange::Ticks(0, 4 * PPQ), tail_seconds: 0.5, ..RenderOptions::default() };
    let r = crate::export::render(&b.p, &FallbackFactory, &opts, &[a, bb], None).unwrap();
    assert_eq!(r.stems.len(), 2);
    let (s0, s1) = (&r.stems[0].2, &r.stems[1].2);
    assert_eq!(s0.frames(), r.mix.frames());
    assert_eq!(s0.frames(), 4 * BEAT + BEAT);
    assert_eq!(first_nonzero(s0, 0), Some(2 * BEAT));
    assert_eq!(first_nonzero(s1, 0), Some(2 * BEAT));
    for k in 0..s0.frames() {
        assert!((s0.channel(0)[k] - s1.channel(0)[k]).abs() < 1e-5, "stems differ at {k}");
    }
}

#[test]
fn rack_chains_are_latency_aligned() {
    let mut b = P::new(120.0);
    let t = b.track(TrackKind::Instrument, "rack");
    let mut rack = Rack::new(RackKind::Instrument);
    for (i, lat) in [0u32, 300].iter().enumerate() {
        let mut ch = Chain::new(format!("c{i}"), Color(0));
        let sid = b.p.alloc_device_id();
        let mut s = Device::stock(sid, TEST_SYNTH, "s");
        s.params.insert(0, 1.0);
        ch.devices.push(s);
        if *lat > 0 {
            let lid = b.p.alloc_device_id();
            let mut l = Device::stock(lid, TEST_LATENCY, "l");
            l.state = Some(lat.to_le_bytes().to_vec());
            ch.devices.push(l);
        }
        rack.chains.push(ch);
    }
    let rid = b.p.alloc_device_id();
    b.t(t).devices.push(Device { kind: DeviceKind::Rack(rack), ..Device::stock(rid, "rack", "Rack") });
    let c = b.notes(&[(60, PPQ, PPQ)], 2 * PPQ, None);
    b.place(t, c, 0);
    let g = compile_plain(&b.p).unwrap();
    assert_eq!(g.total_latency(), 300);
    let out = render(&b.p, (0, 2 * PPQ));
    // Both chains (one delayed internally, one compensated) hit together.
    assert_eq!(onsets(out.channel(0), 100), vec![BEAT]);
}

// ───────────────────────────── Routing ─────────────────────────────

#[test]
fn routing_cycles_are_rejected() {
    let mut b = P::new(120.0);
    let x = b.track(TrackKind::Audio, "x");
    let y = b.track(TrackKind::Audio, "y");
    b.t(x).output = Output::Track(y);
    b.t(y).output = Output::Track(x);
    match compile_plain(&b.p) {
        Err(CompileError::Cycle(ids)) => {
            assert!(ids.contains(&x) && ids.contains(&y));
        }
        other => panic!("expected a cycle, got {:?}", other.err()),
    }
    // A send loop through a return is a cycle too.
    let mut b = P::new(120.0);
    let x = b.track(TrackKind::Audio, "x");
    let r = b.track(TrackKind::Return, "r");
    b.t(x).sends.push(Send { target: r, level_db: 0.0, pre_fader: false });
    b.t(r).sends.push(Send { target: x, level_db: 0.0, pre_fader: false });
    assert!(matches!(compile_plain(&b.p), Err(CompileError::Cycle(_))));
    // Routing into a folder is invalid.
    let mut b = P::new(120.0);
    let f = b.track(TrackKind::Folder, "f");
    let x = b.track(TrackKind::Audio, "x");
    b.t(x).output = Output::Track(f);
    assert!(matches!(compile_plain(&b.p), Err(CompileError::InvalidRoute { .. })));
}

fn silenced(p: &Project, id: TrackId) -> bool {
    let g = compile_plain(p).unwrap();
    g.tracks[g.track_index(id).unwrap()].silenced
}

#[test]
fn solo_logic() {
    let mut b = P::new(120.0);
    let g = b.track(TrackKind::Group, "group");
    let child = b.track(TrackKind::Audio, "child");
    b.t(child).parent = Some(g);
    let other = b.track(TrackKind::Audio, "other");
    let ret = b.track(TrackKind::Return, "reverb");
    let sender = b.track(TrackKind::Audio, "sender");
    b.t(sender).sends.push(Send { target: ret, level_db: 0.0, pre_fader: false });
    b.t(other).sends.push(Send { target: ret, level_db: 0.0, pre_fader: false });

    // Nothing soloed: all audible.
    for id in [g, child, other, ret, sender] {
        assert!(!silenced(&b.p, id));
    }
    // Solo the group: its child plays, unrelated tracks do not.
    b.t(g).solo = true;
    assert!(!silenced(&b.p, g));
    assert!(!silenced(&b.p, child));
    assert!(silenced(&b.p, other));
    assert!(silenced(&b.p, ret));
    // Solo a sender: its return plays (downstream); the other sender is silenced.
    b.t(g).solo = false;
    b.t(sender).solo = true;
    assert!(!silenced(&b.p, sender));
    assert!(!silenced(&b.p, ret));
    assert!(silenced(&b.p, other));
    assert!(silenced(&b.p, child));
    // VCA solo solos its members.
    b.t(sender).solo = false;
    let vca = b.track(TrackKind::Vca, "vca");
    b.t(other).vca = Some(vca);
    b.t(vca).solo = true;
    assert!(!silenced(&b.p, other));
    assert!(silenced(&b.p, sender));
}

#[test]
fn mute_solo_and_vca_in_audio() {
    let mut b = P::new(120.0);
    let (x, _) = b.synth_track("x");
    let (y, _) = b.synth_track("y");
    let c = b.notes(&[(60, 0, PPQ)], PPQ, None);
    b.place(x, c, 0);
    let c2 = b.notes(&[(72, 0, PPQ)], PPQ, None);
    b.place(y, c2, 0);
    let both = render(&b.p, (0, PPQ));
    let mut only_x = b.p.clone();
    only_x.tracks.retain(|t| t.id != y);
    let ref_x = render(&only_x, (0, PPQ));

    let mut solo = b.p.clone();
    solo.track_mut(x).unwrap().solo = true;
    let out = render(&solo, (0, PPQ));
    assert_eq!(out.channel(0), ref_x.channel(0));

    let mut mute = b.p.clone();
    mute.track_mut(y).unwrap().mute = true;
    assert_eq!(render(&mute, (0, PPQ)).channel(0), ref_x.channel(0));
    assert_ne!(both.channel(0), ref_x.channel(0));

    // A VCA at −6 dB scales its member.
    let mut v = only_x.clone();
    let vid = v.alloc_track_id();
    let mut vca = Track::new(vid, TrackKind::Vca, "vca", Color(0));
    vca.volume_db = -6.0;
    v.tracks.push(vca);
    v.track_mut(x).unwrap().vca = Some(vid);
    let out = render(&v, (0, PPQ));
    let ratio = peak(&out.channel(0)[BEAT / 2..]) / peak(&ref_x.channel(0)[BEAT / 2..]);
    assert!((ratio - aitrack_dsp::db_to_gain(-6.0)).abs() < 0.01, "{ratio}");
}

#[test]
fn group_and_return_routing_sum_signals() {
    let mut b = P::new(120.0);
    let grp = b.track(TrackKind::Group, "grp");
    let (x, _) = b.synth_track("x");
    b.t(x).parent = Some(grp);
    let ret = b.track(TrackKind::Return, "ret");
    b.t(x).sends.push(Send { target: ret, level_db: 0.0, pre_fader: false });
    let c = b.notes(&[(60, 0, PPQ)], PPQ, None);
    b.place(x, c, 0);
    let out = render(&b.p, (0, PPQ));
    let mut dry = b.p.clone();
    dry.track_mut(x).unwrap().sends.clear();
    let dry_out = render(&dry, (0, PPQ));
    // Group passes the child through; the send doubles it via the return.
    let k = BEAT / 2;
    assert!((out.channel(0)[k] - 2.0 * dry_out.channel(0)[k]).abs() < 1e-4);
    // Group at −inf silences the child.
    let mut muted = b.p.clone();
    muted.track_mut(grp).unwrap().mute = true;
    muted.track_mut(x).unwrap().sends.clear();
    assert!(peak(render(&muted, (0, PPQ)).channel(0)) < 1e-6);
}

#[test]
fn processors_survive_recompiles() {
    let mut b = P::new(120.0);
    let (t, _) = b.synth_track("x");
    let mut pool = ProcessorPool::new();
    let samples = SampleCache::blocking();
    let mut go = |p: &Project| {
        compile(
            p,
            &mut CompileContext {
                factory: &FallbackFactory,
                pool: &mut pool,
                samples: &samples,
                sample_rate: SR,
                max_frames: 256,
                prev_meters: None,
                taps: Vec::new(),
                analysis: None,
            },
        )
        .unwrap()
    };
    let proc_of = |g: &crate::CompiledGraph| match &g.tracks[g.track_index(t).unwrap()].devices[0] {
        DeviceNode::Proc(p) => (p.proc.clone(), p.init_params.clone()),
        _ => panic!(),
    };
    let g1 = go(&b.p);
    b.t(t).volume_db = -3.0;
    b.t(t).devices[0].params.insert(5, 0.25);
    let g2 = go(&b.p);
    let (p1, _) = proc_of(&g1);
    let (p2, init) = proc_of(&g2);
    assert!(Arc::ptr_eq(&p1, &p2), "same instance reused");
    assert_eq!(init, vec![(5, 0.25)], "only the changed param is re-sent");
    // Changing the device kind builds a new one.
    b.t(t).devices[0].kind = DeviceKind::Stock { plugin_id: TEST_GAIN.into() };
    let g3 = go(&b.p);
    assert!(!Arc::ptr_eq(&p1, &proc_of(&g3).0));
}

// ───────────────────────────── Audio clips ─────────────────────────────

#[test]
fn audio_clip_plays_with_gain_and_warp() {
    let mut b = P::new(120.0);
    let t = b.track(TrackKind::Audio, "audio");
    let mut data = vec![0.0f32; 48_000];
    data[1000] = 1.0;
    let path = std::path::PathBuf::from("mem://impulse.wav");
    let samples = SampleCache::blocking();
    samples.insert(path.clone(), Arc::new(DecodedAudio::from_planar(vec![data], 48_000)));
    let sid = b.p.alloc_source_id();
    let mut warp = Warp::default();
    b.p.sources.insert(
        sid,
        Source::Audio(AudioSource { path: path.clone(), hash: None, sample_rate: 48_000, channels: 1, frames: 48_000, warp: warp.clone(), key: None }),
    );
    let cid = b.p.alloc_clip_id();
    b.p.clips.insert(
        cid,
        Clip { gain_db: -6.0, ..Clip::new("a", Color(0), sid, 2 * PPQ) },
    );
    b.place(t, cid, PPQ);
    let out = render_offline_with(&b.p, &FallbackFactory, (0, 4 * PPQ), SR, 256, samples.clone()).unwrap();
    assert_eq!(first_nonzero(&out, 0), Some(BEAT + 1000));
    assert!((out.channel(0)[BEAT + 1000] - aitrack_dsp::db_to_gain(-6.0)).abs() < 1e-4);

    // Repitch warp: material at 60 BPM in a 120 BPM project plays twice as fast.
    warp.enabled = true;
    warp.original_bpm = Some(60.0);
    if let Some(Source::Audio(a)) = b.p.sources.get_mut(&sid) {
        a.warp = warp;
    }
    let out = render_offline_with(&b.p, &FallbackFactory, (0, 4 * PPQ), SR, 256, samples).unwrap();
    // The sinc interpolator rings a few samples around the impulse, so look
    // for the peak rather than the first non-zero sample.
    let ch = out.channel(0);
    let peak = (0..ch.len()).max_by(|a, b| ch[*a].abs().total_cmp(&ch[*b].abs())).unwrap();
    assert_eq!(peak, BEAT + 500);
}

#[test]
fn unwarped_audio_clip_is_bit_exact() {
    // 97 BPM puts the clip start on a fractional sample, which used to be
    // read through an interpolator and lose high frequencies.
    let mut b = P::new(97.0);
    let t = b.track(TrackKind::Audio, "audio");
    let mut seed = 0x2545_F491u32;
    let data: Vec<f32> = (0..20_000)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed as f32 / u32::MAX as f32) * 2.0 - 1.0
        })
        .collect();
    let path = std::path::PathBuf::from("mem://noise.wav");
    let samples = SampleCache::blocking();
    samples.insert(path.clone(), Arc::new(DecodedAudio::from_planar(vec![data.clone()], 48_000)));
    let sid = b.p.alloc_source_id();
    b.p.sources.insert(
        sid,
        Source::Audio(AudioSource { path, hash: None, sample_rate: 48_000, channels: 1, frames: 20_000, warp: Warp::default(), key: None }),
    );
    let cid = b.p.alloc_clip_id();
    b.p.clips.insert(
        cid,
        Clip::new("n", Color(0), sid, 8 * PPQ),
    );
    b.place(t, cid, PPQ);
    let out = render_offline_with(&b.p, &FallbackFactory, (0, 4 * PPQ), SR, 256, samples).unwrap();
    let start = b.p.tempo.tick_to_samples(PPQ, SR).round() as usize;
    let got = &out.channel(0)[start..start + 20_000];
    // No fade-in at the start of the file, and every sample identical.
    for (i, (a, e)) in got.iter().zip(&data).enumerate() {
        assert!((a - e).abs() < 1e-6, "sample {i}: {a} vs {e}");
    }
}

#[test]
fn fractional_reads_keep_the_top_octave() {
    // A 15 kHz sine read half a sample off: linear interpolation would lose
    // 5.6 dB here; the sinc interpolator must stay within 0.5 dB.
    let sr = 48_000.0f64;
    let data: Vec<f32> = (0..4096).map(|n| (std::f64::consts::TAU * 15_000.0 * n as f64 / sr).sin() as f32).collect();
    let audio = DecodedAudio::from_planar(vec![data], 48_000);
    let peak = (1000..3000).map(|n| audio.sample(0, n as f64 + 0.5).abs()).fold(0.0f32, f32::max);
    let db = 20.0 * peak.log10();
    assert!(db.abs() < 0.5, "15 kHz at a half-sample offset: {db:.2} dB");
    // And it is accurate: compare against the true value of the sine.
    for n in [1200usize, 1777, 2500] {
        let t = n as f64 + 0.37;
        let want = (std::f64::consts::TAU * 15_000.0 * t / sr).sin() as f32;
        assert!((audio.sample(0, t) - want).abs() < 0.03, "at {t}");
    }
    // DC stays at unity for every phase.
    let dc = DecodedAudio::from_planar(vec![vec![1.0; 256]], 48_000);
    for k in 0..16 {
        assert!((dc.sample(0, 100.0 + k as f64 / 16.0) - 1.0).abs() < 1e-4);
    }
}

// ───────────────────────────── Session View ─────────────────────────────

#[test]
fn session_launch_is_quantized_and_reported() {
    let mut b = P::new(120.0);
    let (t, _) = b.synth_track("x");
    let c = b.notes(&[(60, 0, PPQ / 4)], PPQ, Some((0, PPQ)));
    let scene = b.p.scenes[0].id;
    b.t(t).session_slots.insert(scene, c);
    let (h, proc_) = Engine::new(EngineConfig::headless(SR), Arc::new(FallbackFactory)).unwrap();
    let mut proc_ = proc_.unwrap();
    h.update_project(&b.p);
    h.flush();
    h.play();
    let mut buf = AudioBuffer::new(2, 480);
    let mut all = Vec::new();
    // Run one beat, then launch: it must start at the next bar (beat 4).
    while all.len() < BEAT {
        proc_.process_planar(&mut buf);
        all.extend_from_slice(buf.channel(0));
    }
    h.launch_clip(t, scene);
    proc_.process_planar(&mut buf);
    all.extend_from_slice(buf.channel(0));
    let st = h.clip_states();
    assert_eq!(st.len(), 1);
    assert_eq!(st[0].state, SlotState::Queued);
    while all.len() < 7 * BEAT {
        proc_.process_planar(&mut buf);
        all.extend_from_slice(buf.channel(0));
    }
    assert_eq!(onsets(&all, 100), vec![4 * BEAT, 5 * BEAT, 6 * BEAT]);
    assert_eq!(h.clip_states()[0].state, SlotState::Playing);
    // Stop the track at the next bar (beat 8).
    h.stop_track(t);
    while all.len() < 10 * BEAT {
        proc_.process_planar(&mut buf);
        all.extend_from_slice(buf.channel(0));
    }
    assert_eq!(onsets(&all, 100), vec![4 * BEAT, 5 * BEAT, 6 * BEAT, 7 * BEAT]);
    assert_eq!(h.clip_states()[0].state, SlotState::Stopped);
}

#[test]
fn follow_action_moves_to_next_scene() {
    let mut b = P::new(120.0);
    let (t, _) = b.synth_track("x");
    let c1 = b.notes(&[(60, 0, PPQ / 4)], 4 * PPQ, Some((0, 4 * PPQ)));
    let c2 = b.notes(&[(72, 0, PPQ / 4), (72, PPQ, PPQ / 4)], 4 * PPQ, Some((0, 4 * PPQ)));
    b.p.clips.get_mut(&c1).unwrap().launch.follow = Some(FollowAction::Next { after_bars: 1 });
    b.p.clips.get_mut(&c2).unwrap().launch.follow = Some(FollowAction::Stop { after_bars: 1 });
    let (s1, s2) = (b.p.scenes[0].id, b.p.scenes[1].id);
    b.t(t).session_slots.insert(s1, c1);
    b.t(t).session_slots.insert(s2, c2);
    let (h, proc_) = Engine::new(EngineConfig::headless(SR), Arc::new(FallbackFactory)).unwrap();
    let mut proc_ = proc_.unwrap();
    h.update_project(&b.p);
    h.flush();
    // Launch while stopped: starts immediately and starts the transport.
    h.launch_scene(s1);
    let mut buf = AudioBuffer::new(2, 512);
    let mut all = Vec::new();
    while all.len() < 12 * BEAT {
        proc_.process_planar(&mut buf);
        all.extend_from_slice(buf.channel(0));
    }
    // Bar 1: clip 1 (one note); bar 2: clip 2 (two notes); then stopped.
    assert_eq!(onsets(&all, 100), vec![0, 4 * BEAT, 5 * BEAT]);
}

// ───────────────────────────── Live input & metronome ─────────────────────────────

#[test]
fn live_midi_reaches_armed_track() {
    let mut b = P::new(120.0);
    let (t, _) = b.synth_track("keys");
    b.t(t).arm = true;
    let (h, proc_) = Engine::new(EngineConfig::headless(SR), Arc::new(FallbackFactory)).unwrap();
    let mut proc_ = proc_.unwrap();
    h.update_project(&b.p);
    h.flush();
    let mut buf = AudioBuffer::new(2, 256);
    proc_.process_planar(&mut buf);
    assert!(peak(buf.channel(0)) < 1e-6);
    assert!(h.send_midi(MidiMessage::note_on(0, 60, 100)));
    proc_.process_planar(&mut buf);
    assert!(buf.channel(0)[0].abs() > 0.1, "live note starts at the block start (stopped transport)");
    let m = h.meters(t).unwrap();
    assert!(m.peak[0] > 0.1);
    h.send_midi(MidiMessage::note_off(0, 60));
    proc_.process_planar(&mut buf);
    proc_.process_planar(&mut buf);
    assert!(peak(buf.channel(0)) < 1e-6);
}

#[test]
fn targeted_live_midi_reaches_only_its_track_without_changing_external_routing() {
    let mut b = P::new(120.0);
    let (first, _) = b.synth_track("first");
    let (second, _) = b.synth_track("second");
    b.t(first).arm = true;
    let (h, proc_) = Engine::new(EngineConfig::headless(SR), Arc::new(FallbackFactory)).unwrap();
    let mut proc_ = proc_.unwrap();
    h.update_project(&b.p);
    h.flush();
    let mut buf = AudioBuffer::new(2, 256);
    proc_.process_planar(&mut buf);
    assert!(h.send_midi_to(second, MidiMessage::note_on(0, 60, 100)));
    proc_.process_planar(&mut buf);
    assert!(h.meters(second).unwrap().peak[0] > 0.1);
    assert!(h.meters(first).unwrap().peak[0] < 1e-6);
    assert!(h.send_midi_to(second, MidiMessage::note_off(0, 60)));
    proc_.process_planar(&mut buf);
    proc_.process_planar(&mut buf);
    assert!(peak(buf.channel(0)) < 1e-6);
    assert!(h.send_midi_to(TrackId(u64::MAX), MidiMessage::note_on(0, 60, 100)));
    proc_.process_planar(&mut buf);
    assert!(peak(buf.channel(0)) < 1e-6);
    assert!(h.send_midi(MidiMessage::note_on(0, 60, 100)));
    proc_.process_planar(&mut buf);
    assert!(h.meters(first).unwrap().peak[0] > 0.1);
}

#[test]
fn metronome_clicks_on_beats() {
    let b = P::new(120.0);
    let (h, proc_) = Engine::new(EngineConfig::headless(SR), Arc::new(FallbackFactory)).unwrap();
    let mut proc_ = proc_.unwrap();
    h.update_project(&b.p);
    h.flush();
    h.set_metronome(true);
    h.play();
    let mut buf = AudioBuffer::new(2, 1000);
    let mut all = Vec::new();
    while all.len() < 3 * BEAT {
        proc_.process_planar(&mut buf);
        all.extend_from_slice(buf.channel(0));
    }
    // Clicks start on beats (the first sample of a sine burst is 0, so the
    // onset is one sample later).
    assert_eq!(onsets(&all, 2000), vec![1, BEAT + 1, 2 * BEAT + 1]);
    assert!(h.is_playing());
    assert!(h.position_ticks() > 2.9 * PPQ as f64);
}

#[test]
fn count_in_delays_playback() {
    let mut b = P::new(120.0);
    let (t, _) = b.synth_track("x");
    let c = b.notes(&[(60, 0, PPQ / 4)], PPQ, None);
    b.place(t, c, 0);
    let (h, proc_) = Engine::new(EngineConfig::headless(SR), Arc::new(FallbackFactory)).unwrap();
    let mut proc_ = proc_.unwrap();
    h.update_project(&b.p);
    h.flush();
    h.set_count_in(1);
    h.play();
    let mut buf = AudioBuffer::new(2, 700);
    let mut all = Vec::new();
    while all.len() < 6 * BEAT {
        proc_.process_planar(&mut buf);
        all.extend_from_slice(buf.channel(0));
    }
    // One 4/4 bar of count-in (metronome off → silent), then the note.
    assert_eq!(onsets(&all, 100), vec![4 * BEAT]);
}

// ───────────────────────────── Zero allocation ─────────────────────────────

fn busy_project() -> (Project, Vec<TrackId>, SceneId) {
    let mut b = P::new(128.0);
    let ret = b.track(TrackKind::Return, "fx");
    b.device(ret, TEST_GAIN, &[(0, 0.4)], None);
    let grp = b.track(TrackKind::Group, "bus");
    b.device(grp, TEST_LATENCY, &[], Some(128u32.to_le_bytes().to_vec()));
    let vca = b.track(TrackKind::Vca, "vca");
    let scene = b.p.scenes[0].id;
    let mut ids = Vec::new();
    for i in 0..8 {
        let (t, synth) = b.synth_track(&format!("synth {i}"));
        b.device(t, TEST_GAIN, &[(0, 0.5)], None);
        if i % 2 == 0 {
            b.device(t, TEST_LATENCY, &[], Some((64 * i as u32).to_le_bytes().to_vec()));
            b.t(t).parent = Some(grp);
        }
        b.t(t).vca = Some(vca);
        b.t(t).arm = i == 0;
        b.t(t).sends.push(Send { target: ret, level_db: -6.0, pre_fader: i % 3 == 0 });
        let notes: Vec<(u8, Tick, Tick)> = (0..16).map(|k| (48 + (k * 7 + i) as u8 % 36, k as Tick * PPQ / 4, PPQ / 8)).collect();
        let c = b.notes(&notes, 16 * PPQ, Some((0, 4 * PPQ)));
        b.p.clips.get_mut(&c).unwrap().launch.quantize = Quantize::Beat;
        b.place(t, c, 0);
        b.t(t).session_slots.insert(scene, c);
        let mut vol = AutomationLane::new(AutomationTarget::Volume);
        vol.insert(AutomationPoint { tick: 0, value: 0.5, curve: 0.0 });
        vol.insert(AutomationPoint { tick: 8 * PPQ, value: 0.9, curve: 0.3 });
        b.t(t).automation.push(vol);
        let mut prm = AutomationLane::new(AutomationTarget::DeviceParam { device: synth, param: 5 });
        prm.insert(AutomationPoint { tick: 0, value: 0.2, curve: 0.0 });
        prm.insert(AutomationPoint { tick: 4 * PPQ, value: 0.6, curve: 0.0 });
        b.t(t).automation.push(prm);
        ids.push(t);
    }
    // A drum rack with two pads.
    let drums = b.track(TrackKind::Instrument, "drums");
    let mut rack = Rack::new(RackKind::Drum);
    for (k, note) in [36u8, 38].iter().enumerate() {
        let mut ch = Chain::new(format!("pad {k}"), Color(0));
        ch.note = Some(*note);
        let did = b.p.alloc_device_id();
        ch.devices.push(Device::stock(did, TEST_SYNTH, "pad"));
        rack.chains.push(ch);
    }
    let rid = b.p.alloc_device_id();
    b.t(drums).devices.push(Device { kind: DeviceKind::Rack(rack), ..Device::stock(rid, "rack", "Drums") });
    let c = b.notes(&[(36, 0, PPQ / 4), (38, PPQ, PPQ / 4), (36, 2 * PPQ, PPQ / 4)], 4 * PPQ, Some((0, 4 * PPQ)));
    b.place(drums, c, 0);
    let mut tempo = AutomationLane::new(AutomationTarget::Tempo);
    tempo.insert(AutomationPoint { tick: 0, value: crate::automation::tempo_to_normalized(120.0), curve: 0.0 });
    tempo.insert(AutomationPoint { tick: 16 * PPQ, value: crate::automation::tempo_to_normalized(140.0), curve: 0.0 });
    let main = b.main();
    b.t(main).automation.push(tempo);
    ids.push(drums);
    (b.p, ids, scene)
}

#[test]
fn analysis_tap_follows_the_main_output() {
    let mut b = P::new(120.0);
    let (t, _) = b.synth_track("synth");
    let gain = b.device(t, TEST_GAIN, &[(0, 0.25)], None);
    let c = b.notes(&[(60, 0, 4 * PPQ)], 4 * PPQ, None);
    b.place(t, c, 0);
    let (h, proc_) = Engine::new(EngineConfig::headless(SR), Arc::new(FallbackFactory)).unwrap();
    let mut proc_ = proc_.unwrap();
    h.update_project(&b.p);
    h.flush();
    h.play();
    let mut tap = h.analysis_tap();
    let mut buf = AudioBuffer::new(2, 256);
    let mut frames = Vec::new();
    // Off: nothing is written.
    proc_.process_planar(&mut buf);
    assert_eq!(tap.read(&mut frames), 0);
    assert!(frames.is_empty());
    tap.set_enabled(true);
    let mut expect: Vec<[f32; 2]> = Vec::new();
    let mut meter = [0.0f32; 4];
    for _ in 0..40 {
        h.device_meter(gain, &mut meter);
        proc_.process_planar(&mut buf);
        expect.extend(buf.channel(0).iter().zip(buf.channel(1)).map(|(l, r)| [*l, *r]));
    }
    assert_eq!(tap.available(), expect.len());
    assert_eq!(tap.read(&mut frames), 0);
    assert_eq!(frames, expect);
    assert!(frames.iter().any(|f| f[0].abs() > 1e-3));
    assert_eq!(h.device_meter(gain, &mut meter), 2);
    assert_eq!(meter[..2], [0.25, 42.0]);
    assert_eq!(h.device_meter(DeviceId(9_999), &mut meter), 0, "unknown device");
    // A track tap: the synth track post-fader (the Main output is not written).
    tap.set_source(Some(t));
    frames.clear();
    for _ in 0..4 {
        proc_.process_planar(&mut buf);
    }
    assert_eq!(tap.read(&mut frames), 0);
    assert_eq!(frames.len(), 4 * 256);
    assert!(frames.iter().any(|f| f[0].abs() > 1e-3));
    // Disabled again: the ring stops moving.
    tap.set_enabled(false);
    proc_.process_planar(&mut buf);
    assert_eq!(tap.available(), 0);
}

#[test]
fn allocation_counter_counts() {
    let (v, n) = count_allocs(|| vec![1u8; 64]);
    assert!(n >= 1);
    let ((), n) = count_allocs(|| drop(v));
    assert_eq!(n, 1);
}

#[test]
fn audio_thread_never_allocates() {
    let (mut project, ids, scene) = busy_project();
    let (h, proc_) = Engine::new(EngineConfig::headless(SR), Arc::new(FallbackFactory)).unwrap();
    let mut proc_ = proc_.unwrap();
    h.update_project(&project);
    h.flush();
    h.set_loop(Some((0, 8 * PPQ)));
    h.set_metronome(true);
    h.play();
    let mut buf = AudioBuffer::new(2, 256);
    let mut interleaved = vec![0.0f32; 2 * 300];
    // Warm-up block installs the first graph.
    proc_.process_planar(&mut buf);
    // The UI's analysis tap and device meters are live for the whole test.
    let mut tap = h.analysis_tap();
    tap.set_enabled(true);
    let gain_devices: Vec<DeviceId> =
        project.tracks.iter().flat_map(|t| &t.devices).filter(|d| matches!(&d.kind, DeviceKind::Stock { plugin_id } if plugin_id == TEST_GAIN)).map(|d| d.id).collect();
    assert!(!gain_devices.is_empty());
    let mut meter_out = [0.0f32; 8];
    let watch = |h: &crate::EngineHandle, out: &mut [f32; 8]| {
        for d in &gain_devices {
            h.device_meter(*d, out);
        }
    };
    watch(&h, &mut meter_out);

    let run = |proc_: &mut crate::AudioProcessor, buf: &mut AudioBuffer, interleaved: &mut [f32], blocks: usize| {
        count_allocs(|| {
            for i in 0..blocks {
                if i % 3 == 0 {
                    proc_.process_interleaved(interleaved, 2);
                } else {
                    proc_.process_planar(buf);
                }
            }
        })
        .1
    };
    assert_eq!(run(&mut proc_, &mut buf, &mut interleaved, 400), 0, "steady-state playback");
    let mut frames = Vec::new();
    assert!(tap.read(&mut frames) > 0, "400 blocks overran the unread ring");
    assert_eq!(frames.len(), tap.capacity());
    assert!(frames.iter().any(|f| f[0] != 0.0), "the tap carries the mix");
    assert_eq!(h.device_meter(gain_devices[0], &mut meter_out), 2);
    assert_eq!(meter_out[1], 42.0, "device meter values reach the UI side");
    // Tap one track instead of the Main output for the next window.
    tap.set_source(Some(ids[1]));
    watch(&h, &mut meter_out);

    // A recompiled graph is swapped in (state adopted, old one retired).
    project.track_mut(ids[1]).unwrap().volume_db = -4.0;
    project.track_mut(ids[2]).unwrap().devices[0].params.insert(5, 0.8);
    h.update_project(&project);
    h.flush();
    h.launch_scene(scene);
    h.send_midi(MidiMessage::note_on(0, 64, 90));
    h.send_midi_to(ids[1], MidiMessage::note_on(0, 67, 90));
    h.seek(3 * PPQ);
    assert_eq!(run(&mut proc_, &mut buf, &mut interleaved, 400), 0, "graph swap, launches, MIDI, seek");
    {
        // The swap really happened inside the counted window.
        let g = proc_.graph_mut().unwrap();
        let i = g.track_index(ids[1]).unwrap();
        assert_eq!(g.tracks[i].volume_db, -4.0);
        assert!(g.tracks.iter().any(|t| t.session.playing.is_some()), "scene launched");
    }

    h.stop_track(ids[0]);
    h.pause();
    h.play();
    h.stop();
    frames.clear();
    tap.read(&mut frames);
    assert!(frames.iter().any(|f| f[0] != 0.0), "the track tap carries signal");
    tap.set_source(None);
    watch(&h, &mut meter_out);
    assert_eq!(run(&mut proc_, &mut buf, &mut interleaved, 100), 0, "transport changes");
    assert!(h.meters(ids[0]).is_some());
}

// ───────────────────────────── Export ─────────────────────────────

fn tmp(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("aitrack-engine-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

fn sine(frames: usize, amp: f32) -> AudioBuffer {
    let mut b = AudioBuffer::new(2, frames);
    for ch in 0..2 {
        for (i, s) in b.channel_mut(ch).iter_mut().enumerate() {
            *s = amp * (i as f32 * 0.01).sin();
        }
    }
    b
}

#[test]
fn wav_bit_depths_round_trip() {
    let src = sine(4800, 0.8);
    for (fmt, bits, tol) in [
        (WavFormat::Int16(Dither::None), 16, 1.0 / 32_767.0),
        (WavFormat::Int16(Dither::Tpdf), 16, 2.0 / 32_767.0),
        (WavFormat::Int24, 24, 1.0 / 8_388_607.0),
        (WavFormat::Float32, 32, 0.0),
    ] {
        let path = tmp(&format!("depth{bits}-{fmt:?}.wav").replace([' ', '(', ')'], ""));
        write_wav(&path, &src, 48_000, fmt).unwrap();
        let mut r = hound::WavReader::open(&path).unwrap();
        let spec = r.spec();
        assert_eq!(spec.bits_per_sample, bits);
        assert_eq!(spec.channels, 2);
        let back: Vec<f32> = match spec.sample_format {
            hound::SampleFormat::Float => r.samples::<f32>().map(Result::unwrap).collect(),
            hound::SampleFormat::Int => {
                let scale = ((1i64 << (bits - 1)) - 1) as f32;
                r.samples::<i32>().map(|s| s.unwrap() as f32 / scale).collect()
            }
        };
        assert_eq!(back.len(), 4800 * 2);
        for (i, v) in back.iter().enumerate() {
            let x = src.channel(i % 2)[i / 2];
            assert!((v - x).abs() <= tol + 1e-7, "{fmt:?} sample {i}: {v} vs {x}");
        }
    }
}

#[test]
fn dither_only_at_16_bit() {
    // A constant 0.3-LSB (16-bit) signal.
    let mut src = AudioBuffer::new(2, 20_000);
    for ch in 0..2 {
        src.channel_mut(ch).fill(0.3 / 32_767.0);
    }
    let read16 = |fmt: WavFormat| -> Vec<i32> {
        let path = tmp(&format!("dither-{fmt:?}.wav").replace([' ', '(', ')'], ""));
        write_wav(&path, &src, 48_000, fmt).unwrap();
        hound::WavReader::open(&path).unwrap().samples::<i32>().map(Result::unwrap).collect()
    };
    let plain = read16(WavFormat::Int16(Dither::None));
    assert!(plain.iter().all(|s| *s == 0), "undithered rounds to 0");
    for d in [Dither::Tpdf, Dither::TpdfShaped] {
        let dithered = read16(WavFormat::Int16(d));
        let distinct: std::collections::BTreeSet<i32> = dithered.iter().copied().collect();
        assert!(distinct.len() >= 3, "{d:?} adds noise: {distinct:?}");
        assert!(dithered.iter().all(|s| s.abs() <= 3));
        let mean = dithered.iter().map(|s| *s as f64).sum::<f64>() / dithered.len() as f64;
        assert!((mean - 0.3).abs() < 0.05, "{d:?} keeps the average level ({mean})");
    }
    // 24-bit: no dither, constant output.
    let v24 = read16(WavFormat::Int24);
    assert!(v24.windows(2).all(|w| w[0] == w[1]));
}

#[test]
fn export_resamples_to_the_requested_rate() {
    let mut b = P::new(120.0);
    let (t, _) = b.synth_track("x");
    let c = b.notes(&[(60, 0, 2 * PPQ)], 2 * PPQ, None);
    b.place(t, c, 0);
    let opts = RenderOptions {
        range: RenderRange::Ticks(0, 2 * PPQ),
        session_rate: 48_000.0,
        export_rate: 44_100,
        tail_seconds: 0.25,
        ..RenderOptions::default()
    };
    let r = crate::export::render(&b.p, &FallbackFactory, &opts, &[t], None).unwrap();
    let in_frames = 2 * BEAT + 12_000;
    let expected = (in_frames as u64 * 44_100).div_ceil(48_000) as usize;
    assert_eq!(r.sample_rate, 44_100);
    assert_eq!(r.mix.frames(), expected);
    assert_eq!(r.stems[0].2.frames(), expected);

    let path = tmp("resampled.wav");
    export_wav(&b.p, &FallbackFactory, &path, &opts, WavFormat::Int24, None).unwrap();
    let rd = hound::WavReader::open(&path).unwrap();
    assert_eq!(rd.spec().sample_rate, 44_100);
    assert_eq!(rd.duration() as usize, expected);
}

#[test]
fn export_options_normalize_cancel_and_stems() {
    let mut b = P::new(120.0);
    let (x, _) = b.synth_track("Lead / Synth");
    let (y, _) = b.synth_track("Bass");
    let c = b.notes(&[(60, 0, PPQ)], PPQ, None);
    b.place(x, c, 0);
    b.place(y, c, 0);
    let mut opts = RenderOptions { range: RenderRange::Ticks(0, PPQ), tail_seconds: 0.1, normalize_db: Some(-1.0), ..RenderOptions::default() };
    let r = crate::export::render(&b.p, &FallbackFactory, &opts, &[], None).unwrap();
    let p = r.mix.peak(0).max(r.mix.peak(1));
    assert!((p - aitrack_dsp::db_to_gain(-1.0)).abs() < 1e-4, "normalized peak {p}");

    // Progress is reported and cancel stops the render.
    let mut calls = 0;
    let mut cb = |f: f32| {
        calls += 1;
        f < 0.5
    };
    let res = crate::export::render(&b.p, &FallbackFactory, &opts, &[], Some(&mut cb));
    assert!(matches!(res, Err(crate::EngineError::Cancelled)));
    assert!(calls > 1);

    opts.normalize_db = None;
    let dir = tmp("stems");
    let paths = export_stems(&b.p, &FallbackFactory, &dir, &opts, &[x, y], WavFormat::Int16(Dither::Tpdf), None).unwrap();
    assert_eq!(paths.len(), 2);
    assert!(paths[0].file_name().unwrap().to_string_lossy().starts_with("01 Lead _ Synth"));
    let lens: Vec<u32> = paths.iter().map(|p| hound::WavReader::open(p).unwrap().duration()).collect();
    assert_eq!(lens[0], lens[1]);
    assert_eq!(lens[0] as usize, BEAT + 4800);
    // Loop range uses the project's loop region; an empty selection errors.
    opts.range = RenderRange::Ticks(PPQ, PPQ);
    assert!(crate::export::render(&b.p, &FallbackFactory, &opts, &[], None).is_err());
}

#[test]
fn audio_config_serializes() {
    let cfg = crate::AudioConfig {
        host: Some("WASAPI".into()),
        output_device: Some("wasapi:{abc}".into()),
        sample_rate: Some(96_000),
        buffer_size: Some(128),
        input_enabled: true,
        input_channels: crate::ChannelSelection { first: 2, count: 1 },
        ..Default::default()
    };
    let json = serde_json::to_string(&cfg).unwrap();
    let back: crate::AudioConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(back, cfg);
    // Missing fields fall back to defaults (old preference files).
    let old: crate::AudioConfig = serde_json::from_str(r#"{"sample_rate":44100}"#).unwrap();
    assert_eq!(old.sample_rate, Some(44_100));
    assert_eq!(old.output_channels, crate::ChannelSelection::default());
}

mod clips;
