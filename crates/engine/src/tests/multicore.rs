//! Multicore processing: the levels of a graph, bit-identical output at any
//! thread count (offline and live), no allocation on the audio thread nor
//! on the workers, and the edge cases: a sidechain that loops back through
//! the routing, the thread count changing while playing, a device that
//! panics on a worker, graphs swapped in while the workers run. A slow,
//! ignored test prints the speedup.

use super::*;
use crate::alloc_counter::{count_allocs_in_pools, WORKER_COUNT_LOCK};
use crate::factory::ProcessorFactory;
use crate::fallback::TEST_KEY;
use crate::multicore::Schedule;
use crate::{AudioProcessor, EngineHandle};
use aitrack_dsp::{Category, ParamInfo, PluginInfo, ProcessContext, Processor};
use std::sync::atomic::Ordering;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const BLOCK: usize = 256;

// ───────────────────────────── Devices ─────────────────────────────

const SLOW: &str = "probe.slow";
const BOMB: &str = "probe.bomb";

/// `probe.slow` burns ~150 µs per block (so a level really spreads over the
/// workers); `probe.bomb` writes a tone and panics on its 6th block, after
/// noting the name of the thread it fell on.
struct Probe {
    info: PluginInfo,
    calls: usize,
    fell_on: Arc<Mutex<Vec<String>>>,
}

impl Processor for Probe {
    fn info(&self) -> &PluginInfo {
        &self.info
    }
    fn prepare(&mut self, _: f64, _: usize) {}
    fn reset(&mut self) {}
    fn process(&mut self, ctx: &mut ProcessContext<'_>) {
        self.calls += 1;
        if self.info.id == SLOW {
            let t0 = Instant::now();
            while t0.elapsed() < Duration::from_micros(150) {
                std::hint::spin_loop();
            }
            return;
        }
        let (l, r) = ctx.buffer.stereo_mut();
        for (i, (a, b)) in l.iter_mut().zip(r.iter_mut()).enumerate() {
            *a += 0.1 * ((i as f32) * 0.05).sin();
            *b += 0.1 * ((i as f32) * 0.07).sin();
        }
        if self.calls == 6 {
            // (Test-only: allocating on the way down is fine here.)
            self.fell_on.lock().unwrap().push(std::thread::current().name().unwrap_or("?").to_owned());
            panic!("injected panic on a DSP thread");
        }
    }
    fn set_param(&mut self, _: u32, _: f32) {}
    fn get_param(&self, _: u32) -> f32 {
        0.0
    }
}

/// Stock devices (`aitrack-stock`), the engine's test nodes and the probes.
#[derive(Default, Clone)]
struct Mixed {
    fell_on: Arc<Mutex<Vec<String>>>,
}

impl ProcessorFactory for Mixed {
    fn create(&self, device: &Device, sr: f64, max: usize) -> Option<Box<dyn Processor>> {
        let DeviceKind::Stock { plugin_id } = &device.kind else { return None };
        let probe = |id: &'static str| {
            let info = PluginInfo { id, name: "Probe", vendor: "AI-Track", category: Category::Effect, tags: &[], params: vec![ParamInfo::new(0, "Unused", 0.0, 1.0, 0.0)], inputs: 2, outputs: 2, has_sidechain: false };
            Some(Box::new(Probe { info, calls: 0, fell_on: self.fell_on.clone() }) as Box<dyn Processor>)
        };
        match plugin_id.as_str() {
            SLOW => probe(SLOW),
            BOMB => probe(BOMB),
            id if id.starts_with("test.") => FallbackFactory.create(device, sr, max),
            id => aitrack_stock::create(id),
        }
    }
}

// ───────────────────────────── Projects ─────────────────────────────

fn lane(target: AutomationTarget, points: &[(Tick, f32)]) -> AutomationLane {
    let mut l = AutomationLane::new(target);
    for &(tick, value) in points {
        l.insert(AutomationPoint { tick, value, curve: 0.2 });
    }
    l
}

fn pattern(b: &mut P, root: u8, step: Tick) -> ClipId {
    let notes: Vec<(u8, Tick, Tick)> = (0..16).map(|k| (root + (k * 5 % 12) as u8, k as Tick * step, step / 2)).collect();
    b.notes(&notes, 16 * step, Some((0, 16 * step)))
}

/// A noise burst in memory for the audio clip (seeded: the same every time).
fn noise(samples: &SampleCache) -> std::path::PathBuf {
    let path = std::path::PathBuf::from("mem://multicore-noise.wav");
    let mut x = 0x1234_5678u32;
    let mut frames = Vec::with_capacity(BEAT * 2);
    for i in 0..BEAT * 2 {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        let env = (-(i as f32) / 6_000.0).exp();
        frames.push((x as f32 / u32::MAX as f32 * 2.0 - 1.0) * 0.5 * env);
    }
    samples.insert(path.clone(), Arc::new(DecodedAudio::from_planar(vec![frames], 48_000)));
    path
}

/// Many tracks of every kind: groups, returns fed by pre- and post-fader
/// sends, a VCA, a compressor and a key device on sidechains, MIDI clips
/// through stock effects, an audio clip, a drum rack, a MIDI effect,
/// volume / pan / parameter automation, an LFO and a tempo change.
/// `samples`: an audio clip and (with `freeze`) a frozen track, both read
/// through it.
fn song(samples: Option<&SampleCache>, freeze: bool) -> (Project, Vec<TrackId>) {
    let mut b = P::new(124.0);
    let verb = b.track(TrackKind::Return, "verb");
    b.device(verb, "fx.reverb", &[], None);
    let echo = b.track(TrackKind::Return, "echo");
    b.device(echo, "fx.delay", &[], None);
    let drums = b.track(TrackKind::Group, "drums");
    b.device(drums, "fx.drumbus", &[], None);
    let music = b.track(TrackKind::Group, "music");
    b.device(music, "fx.eq", &[], None);
    b.device(music, TEST_LATENCY, &[], Some(96u32.to_le_bytes().to_vec()));
    let vca = b.track(TrackKind::Vca, "vca");
    let mut vca_vol = AutomationLane::new(AutomationTarget::Volume);
    vca_vol.insert(AutomationPoint { tick: 0, value: 0.7, curve: 0.0 });
    vca_vol.insert(AutomationPoint { tick: 8 * PPQ, value: 0.85, curve: 0.0 });
    b.t(vca).automation.push(vca_vol);
    let mut ids = Vec::new();

    // Kick: the sidechain source.
    let (kick, _) = b.synth_track("kick");
    b.device(kick, "fx.saturator", &[], None);
    let c = b.notes(&[(36, 0, PPQ / 4), (36, PPQ, PPQ / 4), (36, 2 * PPQ, PPQ / 4), (36, 3 * PPQ, PPQ / 4)], 4 * PPQ, Some((0, 4 * PPQ)));
    b.place(kick, c, 0);
    b.t(kick).parent = Some(drums);
    b.t(kick).sends.push(Send { target: verb, level_db: -14.0, pre_fader: false });
    ids.push(kick);

    // Bass ducked by the kick, its filter automated and LFO'd.
    let (bass, _) = b.synth_track("bass");
    let comp = b.device(bass, "fx.compressor", &[(0, 0.15), (1, 0.8), (10, 1.0)], None);
    b.p.device_mut(comp).unwrap().sidechain = Some(kick);
    let filt = b.device(bass, "fx.filter", &[(2, 0.6)], None);
    b.t(bass).automation.push(lane(AutomationTarget::DeviceParam { device: filt, param: 3 }, &[(0, 0.1), (6 * PPQ, 0.7), (12 * PPQ, 0.2)]));
    let lfo = b.device(bass, "mod.lfo", &[(2, 0.4)], None);
    b.p.device_mut(lfo).unwrap().mods = vec![ModMapping::bipolar(ModTarget::Param { device: filt, param: 2 }, 0.2), ModMapping::bipolar(ModTarget::Pan, 0.3)];
    let c = pattern(&mut b, 36, PPQ / 2);
    b.place(bass, c, 0);
    b.t(bass).vca = Some(vca);
    b.t(bass).parent = Some(music);
    ids.push(bass);

    // A track that plays nothing but the bass, through its sidechain.
    let keyed = b.track(TrackKind::Audio, "keyed");
    let key = b.device(keyed, TEST_KEY, &[], None);
    b.p.device_mut(key).unwrap().sidechain = Some(bass);
    b.device(keyed, "fx.chorus", &[], None);
    b.t(keyed).volume_db = -9.0;
    ids.push(keyed);

    // Pads and leads.
    for i in 0..6usize {
        let (t, synth) = b.synth_track(&format!("pad {i}"));
        b.device(t, "fx.eq", &[], None);
        if i.is_multiple_of(2) {
            b.device(t, "fx.chorus", &[], None);
        } else {
            b.device(t, "fx.pitchshift", &[], None);
        }
        let c = pattern(&mut b, 48 + 2 * i as u8, PPQ / (1 + i as Tick % 3));
        b.place(t, c, 0);
        b.t(t).sends.push(Send { target: verb, level_db: -8.0 - i as f32, pre_fader: i.is_multiple_of(3) });
        b.t(t).sends.push(Send { target: echo, level_db: -12.0, pre_fader: false });
        if i < 4 {
            b.t(t).parent = Some(music);
        }
        if i % 2 == 1 {
            b.t(t).vca = Some(vca);
        }
        b.t(t).automation.push(lane(AutomationTarget::Volume, &[(0, 0.6), (8 * PPQ, 0.85)]));
        b.t(t).automation.push(lane(AutomationTarget::Pan, &[(0, 0.3), (10 * PPQ, 0.7)]));
        b.t(t).automation.push(lane(AutomationTarget::DeviceParam { device: synth, param: 5 }, &[(0, 0.2), (4 * PPQ, 0.6)]));
        ids.push(t);
    }

    // A drum rack.
    let rack_t = b.track(TrackKind::Instrument, "rack");
    let mut rack = Rack::new(RackKind::Drum);
    for (k, note) in [36u8, 38, 42].iter().enumerate() {
        let mut ch = Chain::new(format!("pad {k}"), Color(0));
        ch.note = Some(*note);
        let did = b.p.alloc_device_id();
        ch.devices.push(Device::stock(did, TEST_SYNTH, "pad"));
        rack.chains.push(ch);
    }
    let rid = b.p.alloc_device_id();
    b.t(rack_t).devices.push(Device { kind: DeviceKind::Rack(rack), ..Device::stock(rid, "rack", "Drums") });
    let c = b.notes(&[(36, 0, PPQ / 4), (38, PPQ, PPQ / 4), (42, PPQ / 2, PPQ / 8), (42, 3 * PPQ / 2, PPQ / 8)], 2 * PPQ, Some((0, 2 * PPQ)));
    b.place(rack_t, c, 0);
    b.t(rack_t).parent = Some(drums);
    ids.push(rack_t);

    // An arpeggiated lead.
    let (arp_t, _) = b.synth_track("arp");
    let arp = b.p.alloc_device_id();
    b.t(arp_t).devices.insert(0, Device::stock(arp, "midi.arp", "Arp"));
    b.device(arp_t, "fx.delay", &[], None);
    let c = b.notes(&[(60, 0, 2 * PPQ), (64, 2 * PPQ, 2 * PPQ)], 4 * PPQ, Some((0, 4 * PPQ)));
    b.place(arp_t, c, 0);
    b.t(arp_t).sends.push(Send { target: echo, level_db: -6.0, pre_fader: true });
    ids.push(arp_t);

    // An audio clip.
    if let Some(samples) = samples {
        let path = noise(samples);
        let t = b.track(TrackKind::Audio, "noise");
        b.device(t, "fx.pitchshift", &[], None);
        b.device(t, "fx.filter", &[], None);
        let sid = b.p.alloc_source_id();
        b.p.sources.insert(sid, Source::Audio(AudioSource { path, hash: None, sample_rate: 48_000, channels: 1, frames: (BEAT * 2) as u64, warp: Warp::default(), key: None }));
        let cid = b.p.alloc_clip_id();
        b.p.clips.insert(cid, Clip::new("noise", Color(0), sid, 2 * PPQ));
        for k in 0..4 {
            b.place(t, cid, k * 4 * PPQ);
        }
        b.t(t).automation.push(lane(AutomationTarget::Pan, &[(0, 0.1), (16 * PPQ, 0.9)]));
        ids.push(t);
    }

    let mut tempo = AutomationLane::new(AutomationTarget::Tempo);
    tempo.insert(AutomationPoint { tick: 0, value: crate::automation::tempo_to_normalized(124.0), curve: 0.0 });
    tempo.insert(AutomationPoint { tick: 12 * PPQ, value: crate::automation::tempo_to_normalized(132.0), curve: 0.0 });
    let main = b.main();
    b.t(main).automation.push(tempo);
    b.device(main, "fx.limiter", &[], None);

    // A frozen track: rendered here (blocking cache), played from its file.
    if freeze {
        let samples = samples.expect("a frozen track needs a cache");
        let (t, _) = b.synth_track("frozen");
        b.device(t, "fx.reverb", &[], None);
        let c = pattern(&mut b, 55, PPQ);
        b.place(t, c, 0);
        b.t(t).parent = Some(music);
        let path = std::env::temp_dir().join(format!("aitrack-multicore-{}-frozen.wav", std::process::id()));
        let file = freeze_track_to_wav(&b.p, t, 48_000, &path, Some(samples.clone()), &Mixed::default(), None).expect("freeze renders");
        Command::FreezeTrack { track: t, file }.apply(&mut b.p).expect("freeze applies");
        ids.push(t);
    }
    (b.p, ids)
}

fn graph_of(p: &Project, factory: &dyn ProcessorFactory, samples: &SampleCache) -> crate::CompiledGraph {
    let mut pool = ProcessorPool::new();
    compile(
        p,
        &mut CompileContext { factory, pool: &mut pool, samples, sample_rate: SR, max_frames: BLOCK, prev_meters: None, taps: Vec::new(), analysis: None, streams: None },
    )
    .unwrap()
}

fn render_on(p: &Project, samples: &SampleCache, threads: usize, stems: &[TrackId]) -> Rendered {
    let opts = RenderOptions {
        range: RenderRange::Ticks(0, 16 * PPQ),
        block: BLOCK,
        tail_seconds: 1.0,
        samples: Some(samples.clone()),
        threads: Some(threads),
        ..RenderOptions::default()
    };
    crate::export::render(p, &Mixed::default(), &opts, stems, None).unwrap()
}

#[track_caller]
fn same_bits(a: &AudioBuffer, b: &AudioBuffer, what: &str) {
    assert_eq!((a.channels(), a.frames()), (b.channels(), b.frames()), "{what}: shape");
    for ch in 0..a.channels() {
        if let Some(i) = a.channel(ch).iter().zip(b.channel(ch)).position(|(x, y)| x.to_bits() != y.to_bits()) {
            panic!("{what}: channel {ch} differs first at frame {i}: {} vs {}", a.channel(ch)[i], b.channel(ch)[i]);
        }
    }
}

// ───────────────────────────── Levels ─────────────────────────────

fn level_of(s: &Schedule, g: &crate::CompiledGraph, id: TrackId) -> usize {
    let i = g.track_index(id).unwrap() as u32;
    s.levels.iter().position(|l| l.tracks.contains(&i)).expect("an audio track has a level")
}

#[test]
fn levels_follow_routing_sends_and_sidechains() {
    let samples = SampleCache::blocking();
    let (p, ids) = song(Some(&samples), false);
    let g = graph_of(&p, &Mixed::default(), &samples);
    let s = &g.schedule;
    assert!(s.parallel, "a big project runs in parallel");
    let id = |name: &str| p.tracks.iter().find(|t| t.name == name).unwrap().id;
    let lv = |name: &str| level_of(s, &g, id(name));
    let (kick, bass, keyed) = (ids[0], ids[1], ids[2]);
    assert_eq!(level_of(s, &g, kick), 0);
    assert_eq!(level_of(s, &g, bass), 1, "the compressor listens to the kick");
    assert_eq!(level_of(s, &g, keyed), 2, "the key device listens to the bass");
    assert!(lv("drums") > lv("kick") && lv("drums") > lv("rack"), "a group after its members");
    assert!(lv("music") > lv("bass") && lv("music") > lv("pad 0"));
    assert!(lv("verb") > lv("pad 5") && lv("echo") > lv("arp"), "a return after its senders");
    assert!(lv("Main") > lv("verb") && lv("Main") > lv("drums") && lv("Main") > lv("music"));
    // The VCA carries no audio and has no level; its members do not wait for it.
    assert!(s.levels.iter().all(|l| !l.tracks.contains(&(g.track_index(id("vca")).unwrap() as u32))));
    assert_eq!(lv("pad 1"), 0);
    // Every audio track exactly once.
    let mut all: Vec<u32> = s.levels.iter().flat_map(|l| l.tracks.clone()).collect();
    all.sort();
    let audio: Vec<u32> = (0..g.tracks.len() as u32).filter(|&i| g.tracks[i as usize].carries_audio()).collect();
    assert_eq!(all, audio);
    // Inputs are summed in the order the single path adds them: graph order.
    for f in &s.feeds {
        assert!(f.windows(2).all(|w| w[0].track <= w[1].track));
    }
    assert!(s.levels[0].parallel && s.levels[0].tracks.len() >= 8);
}

/// Project order `listener, loop, source`: `loop` feeds the group whose
/// signal a device of `loop` listens to (a loop through the routing: the
/// graph order follows the routing alone), and `listener` sidechains from
/// `source`, which comes after it. Both sidechains hear the previous block.
fn looped() -> (Project, TrackId, TrackId, TrackId, TrackId) {
    let mut b = P::new(120.0);
    let (listener, _) = b.synth_track("listener");
    let (looper, _) = b.synth_track("loop");
    let (source, _) = b.synth_track("source");
    let grp = b.track(TrackKind::Group, "grp");
    for &t in &[listener, looper, source] {
        let c = pattern(&mut b, 50, PPQ / 2);
        b.place(t, c, 0);
    }
    let k1 = b.device(listener, TEST_KEY, &[], None);
    b.p.device_mut(k1).unwrap().sidechain = Some(source);
    b.t(looper).parent = Some(grp);
    let k2 = b.device(looper, TEST_KEY, &[], None);
    b.p.device_mut(k2).unwrap().sidechain = Some(grp);
    b.device(listener, "fx.eq", &[], None);
    b.device(source, "fx.eq", &[], None);
    (b.p, listener, looper, source, grp)
}

#[test]
fn a_sidechain_loop_keeps_hearing_the_previous_block() {
    let (p, listener, looper, source, grp) = looped();
    let samples = SampleCache::blocking();
    let g = graph_of(&p, &Mixed::default(), &samples);
    assert!(g.warnings.iter().any(|w| w.contains("one block late")), "{:?}", g.warnings);
    let s = &g.schedule;
    let (il, is) = (g.track_index(listener).unwrap(), g.track_index(source).unwrap());
    assert!(il < is, "the listener comes first in the graph");
    assert!(level_of(s, &g, source) > level_of(s, &g, listener), "the source writes this block only after the listener read the last");
    assert!(level_of(s, &g, grp) > level_of(s, &g, looper));
    // The same samples at any thread count (and they are not silent).
    let one = render_on(&p, &samples, 1, &[listener, looper]);
    assert!(peak(one.mix.channel(0)) > 0.01);
    for threads in [2, 4, 8] {
        let many = render_on(&p, &samples, threads, &[listener, looper]);
        same_bits(&one.mix, &many.mix, &format!("mix at {threads} threads"));
        for (a, b) in one.stems.iter().zip(&many.stems) {
            same_bits(&a.2, &b.2, &format!("stem '{}' at {threads} threads", a.1));
        }
    }
}

// ───────────────────────────── Bit-identical renders ─────────────────────────────

#[test]
fn every_thread_count_renders_the_same_samples() {
    let samples = SampleCache::blocking();
    let (p, ids) = song(Some(&samples), true);
    let g = graph_of(&p, &Mixed::default(), &samples);
    assert!(g.schedule.parallel);
    assert!(g.warnings.is_empty(), "{:?}", g.warnings);
    drop(g);
    let stems = [ids[0], ids[1], ids[2], ids[5], *ids.last().unwrap()];
    let one = render_on(&p, &samples, 1, &stems);
    assert!(peak(one.mix.channel(0)) > 0.05 && peak(one.mix.channel(1)) > 0.05, "the song plays");
    assert!(one.stems.iter().all(|s| peak(s.2.channel(0)) > 1e-4), "every stem plays");
    for threads in [2, 4, 8] {
        let many = render_on(&p, &samples, threads, &stems);
        same_bits(&one.mix, &many.mix, &format!("mix at {threads} threads"));
        for (a, b) in one.stems.iter().zip(&many.stems) {
            same_bits(&a.2, &b.2, &format!("stem '{}' at {threads} threads", a.1));
        }
    }
    let _ = std::fs::remove_file(std::env::temp_dir().join(format!("aitrack-multicore-{}-frozen.wav", std::process::id())));
}

// ───────────────────────────── Live ─────────────────────────────

fn live(p: &Project, threads: usize, factory: Mixed) -> (EngineHandle, AudioProcessor) {
    let (h, proc_) = Engine::new(EngineConfig { audio_threads: threads, ..EngineConfig::headless(SR) }, Arc::new(factory)).unwrap();
    h.update_project(p);
    h.flush();
    h.set_loop(Some((0, 8 * PPQ)));
    h.play();
    (h, proc_.unwrap())
}

/// Play `blocks` blocks, calling `step(h, block)` before each; the output.
fn play(h: &EngineHandle, proc_: &mut AudioProcessor, blocks: usize, mut step: impl FnMut(&EngineHandle, &mut AudioProcessor, usize)) -> Vec<f32> {
    let mut out = Vec::with_capacity(blocks * BLOCK * 2);
    let mut buf = AudioBuffer::new(2, BLOCK);
    for k in 0..blocks {
        step(h, proc_, k);
        proc_.process_planar(&mut buf);
        out.extend_from_slice(buf.channel(0));
        out.extend_from_slice(buf.channel(1));
    }
    out
}

#[track_caller]
fn same_vec(a: &[f32], b: &[f32], what: &str) {
    assert_eq!(a.len(), b.len());
    if let Some(i) = a.iter().zip(b).position(|(x, y)| x.to_bits() != y.to_bits()) {
        panic!("{what}: differs first at sample {i} (block {}): {} vs {}", i / (2 * BLOCK), a[i], b[i]);
    }
}

/// What both live runs do: a recompile, a scene launch, a seek, live MIDI.
fn script(p: &Project, ids: &[TrackId]) -> impl FnMut(&EngineHandle, &mut AudioProcessor, usize) {
    let mut edited = p.clone();
    let ids = ids.to_vec();
    move |h, _, k| match k {
        60 => {
            edited.track_mut(ids[3]).unwrap().volume_db = -3.0;
            edited.track_mut(ids[4]).unwrap().mute = true;
            h.update_project(&edited);
            h.flush();
        }
        120 => {
            h.send_midi_to(ids[5], MidiMessage::note_on(0, 67, 100));
            h.seek(2 * PPQ);
        }
        160 => {
            h.send_midi_to(ids[5], MidiMessage::note_off(0, 67));
        }
        _ => {}
    }
}

#[test]
fn live_playback_is_the_same_on_one_thread_and_on_many() {
    let (p, ids) = song(None, false);
    let (h1, mut p1) = live(&p, 1, Mixed::default());
    let one = play(&h1, &mut p1, 240, script(&p, &ids));
    assert!(peak(&one) > 0.05);
    assert_eq!(p1.audio_threads(), 1);
    for threads in [2, 4] {
        let (h, mut proc_) = live(&p, threads, Mixed::default());
        assert_eq!(proc_.audio_threads(), threads);
        let many = play(&h, &mut proc_, 240, script(&p, &ids));
        same_vec(&one, &many, &format!("{threads} threads"));
        assert_eq!(h.audio_threads(), threads);
        assert_eq!(h.audio_health().node_panics, 0);
    }
}

#[test]
fn the_thread_count_changes_while_playing_without_a_glitch() {
    let (p, ids) = song(None, false);
    let (h1, mut p1) = live(&p, 1, Mixed::default());
    let reference = play(&h1, &mut p1, 300, script(&p, &ids));
    let (h, mut proc_) = live(&p, 1, Mixed::default());
    let mut inner = script(&p, &ids);
    let changes = [(40, 4), (90, 2), (140, 1), (200, 0), (250, 3)];
    let out = play(&h, &mut proc_, 300, |h, proc_, k| {
        inner(h, proc_, k);
        if let Some(&(_, n)) = changes.iter().find(|c| c.0 == k) {
            h.set_audio_threads(n);
            h.flush();
        }
        if let Some(&(_, n)) = changes.iter().find(|c| c.0 + 1 == k) {
            assert_eq!(proc_.audio_threads(), crate::resolve_threads(n), "swapped at the block after the change");
        }
    });
    same_vec(&reference, &out, "thread count changes");
    assert_eq!(h.audio_health().callback_panics, 0);
}

#[test]
fn a_device_that_panics_on_a_worker_falls_alone() {
    // Eight heavy tracks, each with a bomb: they spread over the workers.
    let mut b = P::new(120.0);
    let mut bombs = Vec::new();
    for i in 0..8 {
        let (t, _) = b.synth_track(&format!("t{i}"));
        b.device(t, SLOW, &[], None);
        bombs.push(b.device(t, BOMB, &[], None));
        let c = pattern(&mut b, 40 + i as u8, PPQ / 2);
        b.place(t, c, 0);
    }
    let run = |threads: usize| {
        let factory = Mixed::default();
        let (h, mut proc_) = live(&b.p, threads, factory.clone());
        let out = play(&h, &mut proc_, 40, |_, _, _| {});
        (out, h.audio_health(), factory.fell_on.lock().unwrap().clone())
    };
    let (one, health1, _) = run(1);
    let (many, health4, fell_on) = run(4);
    assert_eq!(health1.node_panics, 8);
    assert_eq!(health4.node_panics, 8, "every bomb fell once, and only its own device");
    assert_eq!(health4.callback_panics, 0, "the engine never stopped");
    assert!(fell_on.iter().any(|n| n.starts_with("aitrack-dsp-")), "some fell on a worker: {fell_on:?}");
    same_vec(&one, &many, "falls on workers");
    // It played until the fall; a fallen device silences its chain from then on.
    assert!(peak(&many[..4 * 2 * BLOCK]) > 0.01);
    assert_eq!(peak(&many[many.len() - 2 * BLOCK..]), 0.0);
}

#[test]
fn graphs_swap_in_while_the_workers_run() {
    let (p, ids) = song(None, false);
    let (h, mut proc_) = live(&p, 4, Mixed::default());
    let h = Arc::new(h);
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let editor = {
        let (h, stop, ids) = (h.clone(), stop.clone(), ids.clone());
        let mut p = p.clone();
        let music = p.tracks.iter().find(|t| t.name == "music").unwrap().id;
        std::thread::spawn(move || {
            let mut k = 0usize;
            while !stop.load(Ordering::Relaxed) {
                k += 1;
                // Structure changes: a recompile each time, routing included.
                let t = p.track_mut(ids[3 + k % 5]).unwrap();
                t.parent = if t.parent.is_some() { None } else { Some(music) };
                p.track_mut(ids[1]).unwrap().mute = k.is_multiple_of(3);
                if k.is_multiple_of(4) {
                    let id = p.alloc_track_id();
                    p.tracks.push(Track::new(id, TrackKind::Instrument, format!("x{k}"), Color(0)));
                } else if k % 4 == 2 {
                    p.tracks.retain(|t| !t.name.starts_with('x'));
                }
                h.update_project(&p);
                h.flush();
            }
            k
        })
    };
    let mut buf = AudioBuffer::new(2, BLOCK);
    let mut data = vec![0.0f32; 2 * BLOCK];
    let t0 = Instant::now();
    let mut blocks = 0usize;
    while t0.elapsed() < Duration::from_millis(1500) {
        if blocks.is_multiple_of(2) {
            assert!(proc_.process_interleaved_guarded(&mut data, 2, 0));
            assert!(data.iter().all(|s| s.is_finite()));
        } else {
            proc_.process_planar(&mut buf);
        }
        blocks += 1;
    }
    stop.store(true, Ordering::Relaxed);
    let edits = editor.join().unwrap();
    assert!(edits > 5, "the graph was swapped many times ({edits})");
    let health = h.audio_health();
    assert_eq!((health.callback_panics, health.node_panics), (0, 0));
    assert_eq!(proc_.audio_threads(), 4);
}

// ───────────────────────────── Real-time safety ─────────────────────────────

#[test]
fn neither_the_audio_thread_nor_the_workers_allocate() {
    let _serial = WORKER_COUNT_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let (mut project, ids, scene) = busy_project();
    // A sidechain too, so the bus is exercised.
    let key = project.alloc_device_id();
    let mut d = Device::stock(key, TEST_KEY, "key");
    d.sidechain = Some(ids[0]);
    project.track_mut(ids[1]).unwrap().devices.push(d);
    let (h, proc_) = Engine::new(EngineConfig { audio_threads: 4, ..EngineConfig::headless(SR) }, Arc::new(FallbackFactory)).unwrap();
    let mut proc_ = proc_.unwrap();
    h.update_project(&project);
    h.flush();
    h.set_loop(Some((0, 8 * PPQ)));
    h.set_metronome(true);
    h.play();
    let mut buf = AudioBuffer::new(2, BLOCK);
    let mut interleaved = vec![0.0f32; 2 * 300];
    // Warm-up: installs the graph, wakes every worker once.
    for _ in 0..4 {
        proc_.process_planar(&mut buf);
    }
    assert_eq!(proc_.audio_threads(), 4);
    assert!(proc_.graph_mut().unwrap().schedule.parallel, "the busy project runs in parallel");
    let mut tap = h.analysis_tap();
    tap.set_enabled(true);
    tap.set_source(Some(ids[2]));
    let pool = proc_.pool_id();
    let run = |proc_: &mut AudioProcessor, buf: &mut AudioBuffer, interleaved: &mut [f32], blocks: usize| {
        count_allocs_in_pools(&[pool], || {
            for i in 0..blocks {
                if i.is_multiple_of(3) {
                    proc_.process_interleaved(interleaved, 2);
                } else {
                    proc_.process_planar(buf);
                }
                if i.is_multiple_of(50) {
                    // Long enough for the workers to park between blocks.
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
        })
        .1
    };
    assert_eq!(run(&mut proc_, &mut buf, &mut interleaved, 400), 0, "steady-state playback on 4 threads");

    project.track_mut(ids[1]).unwrap().volume_db = -4.0;
    project.track_mut(ids[2]).unwrap().devices[0].params.insert(5, 0.8);
    h.update_project(&project);
    h.flush();
    h.launch_scene(scene);
    h.send_midi(MidiMessage::note_on(0, 64, 90));
    h.seek(3 * PPQ);
    assert_eq!(run(&mut proc_, &mut buf, &mut interleaved, 400), 0, "graph swap, launches, MIDI, seek on 4 threads");
    let mut frames = Vec::new();
    tap.read(&mut frames);
    assert!(frames.iter().any(|f| f[0] != 0.0), "the tapped track reached the analysis ring");
}

#[test]
fn the_allocation_counter_sees_the_workers() {
    let _serial = WORKER_COUNT_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let mut pool = crate::workers::WorkerPool::new(4, crate::workers::PoolKind::Offline);
    let id = pool.id();
    let on_workers = std::sync::atomic::AtomicUsize::new(0);
    // On a small, busy machine (a 2-core CI runner) the caller can finish a
    // whole run before a worker wakes: run again until one has taken part.
    let ((), n) = count_allocs_in_pools(&[id], || {
        for _ in 0..200 {
            pool.run(64, &|_| {
                let t0 = Instant::now();
                while t0.elapsed() < Duration::from_micros(100) {
                    std::hint::spin_loop();
                }
                if std::thread::current().name().is_some_and(|n| n.starts_with("aitrack-dsp-")) {
                    on_workers.fetch_add(1, Ordering::Relaxed);
                    drop(std::hint::black_box(vec![0u8; 32]));
                }
            });
            if on_workers.load(Ordering::Relaxed) > 0 {
                break;
            }
        }
    });
    let w = on_workers.load(Ordering::Relaxed) as u64;
    assert!(w > 0, "the workers took part");
    assert!(n >= 2 * w, "an allocation and a free per worker job ({n} counted for {w} jobs)");
}

// ───────────────────────────── Benchmark ─────────────────────────────

/// `cargo test -p aitrack-engine --release multicore_speedup -- --ignored --nocapture`
#[test]
#[ignore = "slow: prints the multicore speedup"]
fn multicore_speedup() {
    let mut b = P::new(128.0);
    for i in 0..32 {
        let (t, _) = b.synth_track(&format!("t{i}"));
        for _ in 0..3 {
            b.device(t, "fx.eq", &[], None);
        }
        b.device(t, "fx.reverb", &[], None);
        b.device(t, "fx.pitchshift", &[], None);
        let c = pattern(&mut b, 36 + (i % 24) as u8, PPQ / 2);
        b.place(t, c, 0);
    }
    let samples = SampleCache::blocking();
    let opts = |threads| RenderOptions { range: RenderRange::Ticks(0, 16 * PPQ), block: BLOCK, tail_seconds: 0.0, samples: Some(samples.clone()), threads: Some(threads), ..RenderOptions::default() };
    let audio_seconds = 16.0 * 60.0 / 128.0;
    let mut base = None;
    let mut reference: Option<AudioBuffer> = None;
    // `AITRACK_BENCH_THREADS=8,1,4` picks the counts and their order.
    let counts: Vec<usize> = std::env::var("AITRACK_BENCH_THREADS")
        .ok()
        .map(|v| v.split(',').filter_map(|n| n.trim().parse().ok()).collect())
        .unwrap_or_else(|| vec![1, 2, 4, 6, 8, crate::auto_threads()]);
    eprintln!("32 tracks × (synth, 3 EQ, reverb, pitch shifter), {audio_seconds:.1} s of audio, {BLOCK}-frame blocks; auto = {}", crate::auto_threads());
    for threads in counts {
        // Best of three.
        let mut best = f64::MAX;
        for _ in 0..3 {
            let t0 = Instant::now();
            let r = crate::export::render(&b.p, &Mixed::default(), &opts(threads), &[], None).unwrap();
            best = best.min(t0.elapsed().as_secs_f64());
            match &reference {
                Some(want) => same_bits(want, &r.mix, &format!("{threads} threads")),
                None => reference = Some(r.mix),
            }
        }
        let base = *base.get_or_insert(best);
        eprintln!("{threads:>2} threads: {best:7.3} s  ({:5.1}× real time, speedup {:4.2}×)", audio_seconds / best, base / best);
    }
}
