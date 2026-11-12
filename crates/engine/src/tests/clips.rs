//! Audio clips through the whole engine: fades, the volume envelope, loops
//! and their seams, splits, the Session View, export and the audio thread's
//! zero-allocation guarantee.

use super::*;
use crate::rt::{AudioProcessor, RtCommand, RtQueues, TransportInit};
use crate::status::SharedStatus;
use rtrb::RingBuffer;

/// Samples per tick at 120 BPM and 48 kHz.
const SPT: usize = 25;

/// A mono 48 kHz file that only exists in the sample cache.
fn source(b: &mut P, samples: &SampleCache, name: &str, data: Vec<f32>, warp: Warp) -> SourceId {
    let path = std::path::PathBuf::from(format!("mem://{name}.wav"));
    let frames = data.len() as u64;
    samples.insert(path.clone(), Arc::new(DecodedAudio::from_planar(vec![data], 48_000)));
    let sid = b.p.alloc_source_id();
    b.p.sources.insert(sid, Source::Audio(AudioSource { path, hash: None, sample_rate: 48_000, channels: 1, frames, warp, key: None }));
    sid
}

fn add_clip(b: &mut P, clip: Clip) -> ClipId {
    let id = b.p.alloc_clip_id();
    b.p.clips.insert(id, clip);
    id
}

fn edit(p: &mut Project, cmd: Command) {
    cmd.apply(p).expect("command applies");
}

fn render_with(p: &Project, samples: &SampleCache, ticks: (Tick, Tick)) -> AudioBuffer {
    render_offline_with(p, &FallbackFactory, ticks, SR, 256, samples.clone()).unwrap()
}

fn max_step(data: &[f32]) -> f32 {
    data.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max)
}

/// Distinct value per frame: `(i + 1) / len`.
fn ramp(len: usize) -> Vec<f32> {
    (0..len).map(|i| (i + 1) as f32 / len as f32).collect()
}

/// A 1010 Hz sine. Over 10 000 or 12 000 frames it does not end where it
/// starts, so a loop of it jumps unless the seam is crossfaded.
fn sine(len: usize) -> Vec<f32> {
    (0..len).map(|i| 0.5 * (std::f64::consts::TAU * 1010.0 * i as f64 / 48_000.0 + 0.9).sin() as f32).collect()
}

fn noise(len: usize) -> Vec<f32> {
    let mut seed = 0x2545_F491u32;
    (0..len)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed as f32 / u32::MAX as f32) - 0.5
        })
        .collect()
}

/// One audio track with one clip over `data`, placed at beat 1.
fn one_clip(data: Vec<f32>, clip: impl FnOnce(SourceId) -> Clip) -> (P, SampleCache, TrackId, ClipId) {
    let mut b = P::new(120.0);
    let samples = SampleCache::blocking();
    let t = b.track(TrackKind::Audio, "audio");
    let sid = source(&mut b, &samples, "clip", data, Warp::default());
    let cid = add_clip(&mut b, clip(sid));
    b.place(t, cid, PPQ);
    (b, samples, t, cid)
}

// ───────────────────────────── Fades and envelope ─────────────────────────────

#[test]
fn fades_follow_gain_at() {
    let shapes = [
        (Fade { length: 480, curve: 0.0 }, Fade { length: 720, curve: 0.0 }),
        (Fade { length: 480, curve: 0.8 }, Fade { length: 720, curve: -0.6 }),
        (Fade { length: 480, curve: -1.0 }, Fade { length: 1440, curve: 1.0 }),
    ];
    for (fade_in, fade_out) in shapes {
        let (b, samples, _, cid) = one_clip(vec![1.0; 2 * BEAT], |s| Clip { fade_in, fade_out, ..Clip::new("c", Color(0), s, 2 * PPQ) });
        let clip = &b.p.clips[&cid];
        let out = render_with(&b.p, &samples, (0, 4 * PPQ));
        let ch = out.channel(0);
        for tick in [0, 1, 60, 240, 479, 480, 481, 700, 1199, 1200, 1201, 1500, 1800, 1919] {
            let (got, want) = (ch[BEAT + SPT * tick as usize], clip.gain_at(tick));
            assert!((got - want).abs() < 1e-4, "{fade_in:?} {fade_out:?} tick {tick}: {got} vs {want}");
        }
        // Silent before, ~0 on the first and the last sample, silent after.
        assert_eq!(peak(&ch[..BEAT]), 0.0);
        assert!(ch[BEAT].abs() < 1e-4 && ch[BEAT + 1] > 0.0, "first samples {} {}", ch[BEAT], ch[BEAT + 1]);
        assert!(ch[3 * BEAT - 1].abs() < 1e-3, "last sample {}", ch[3 * BEAT - 1]);
        assert!(peak(&ch[3 * BEAT..]) < 1e-6);
        // Evaluated per sample: a smooth rise and fall, no control-rate steps.
        let rise = &ch[BEAT..BEAT + 480 * SPT];
        assert!(rise.windows(2).all(|w| w[1] >= w[0]), "{fade_in:?}: the fade-in is not monotonic");
        assert!(max_step(&ch[BEAT..3 * BEAT]) < 6e-4, "{fade_in:?} {fade_out:?}: step {}", max_step(&ch[BEAT..3 * BEAT]));
    }
}

#[test]
fn fast_fade_has_no_zipper_noise() {
    // A 4-tick fade is 100 samples: three control blocks. Every sample must
    // step by the same 1/100.
    let fade = Fade { length: 4, curve: 0.0 };
    let (b, samples, ..) = one_clip(vec![1.0; 2 * BEAT], |s| Clip { fade_in: fade, fade_out: fade, ..Clip::new("c", Color(0), s, 2 * PPQ) });
    let out = render_with(&b.p, &samples, (0, 4 * PPQ));
    let ch = out.channel(0);
    for (i, w) in ch[BEAT..BEAT + 101].windows(2).enumerate() {
        assert!((w[1] - w[0] - 0.01).abs() < 1e-4, "fade-in sample {i}: step {}", w[1] - w[0]);
    }
    for (i, w) in ch[3 * BEAT - 100..3 * BEAT].windows(2).enumerate() {
        assert!((w[0] - w[1] - 0.01).abs() < 1e-4, "fade-out sample {i}: step {}", w[0] - w[1]);
    }
    assert!((ch[BEAT + 5000] - 1.0).abs() < 1e-5);
}

#[test]
fn anti_click_ramp_only_where_there_is_no_fade() {
    // A trimmed clip (offset 1 beat) of a constant source.
    let trimmed = |fade_in: Fade, fade_out: Fade| {
        let (b, samples, ..) =
            one_clip(vec![1.0; 4 * BEAT], |s| Clip { offset: PPQ, fade_in, fade_out, ..Clip::new("c", Color(0), s, 2 * PPQ) });
        render_with(&b.p, &samples, (0, 4 * PPQ))
    };
    let (none, short) = (Fade::default(), Fade { length: 4, curve: 0.0 });
    // No fades: the 64-sample ramps at both edges, as before.
    let out = trimmed(none, none);
    let ch = out.channel(0);
    assert!((ch[BEAT + 32] - 0.5).abs() < 1e-4 && (ch[BEAT + 64] - 1.0).abs() < 1e-4);
    assert!((ch[3 * BEAT - 32] - 0.5).abs() < 1e-4 && (ch[3 * BEAT - 65] - 1.0).abs() < 1e-4);
    // A fade-in replaces the ramp at the start (0.32, not 0.32 × 0.5); the
    // end keeps its ramp.
    let out = trimmed(short, none);
    let ch = out.channel(0);
    assert!((ch[BEAT + 32] - 0.32).abs() < 1e-4, "{}", ch[BEAT + 32]);
    assert!((ch[BEAT + 80] - 0.80).abs() < 1e-4, "{}", ch[BEAT + 80]);
    assert!((ch[3 * BEAT - 32] - 0.5).abs() < 1e-4);
    // And a fade-out replaces the ramp at the end.
    let out = trimmed(none, short);
    let ch = out.channel(0);
    assert!((ch[BEAT + 32] - 0.5).abs() < 1e-4);
    assert!((ch[3 * BEAT - 32] - 0.32).abs() < 1e-4, "{}", ch[3 * BEAT - 32]);
    assert!((ch[3 * BEAT - 80] - 0.80).abs() < 1e-4, "{}", ch[3 * BEAT - 80]);
}

#[test]
fn volume_envelope_follows_gain_at() {
    let (b, samples, _, cid) = one_clip(vec![1.0; 2 * BEAT], |s| {
        let mut lane = AutomationLane::new(AutomationTarget::Volume);
        lane.insert(AutomationPoint { tick: 0, value: 1.0, curve: 0.0 });
        lane.insert(AutomationPoint { tick: 480, value: 0.25, curve: 0.7 });
        lane.insert(AutomationPoint { tick: 1440, value: 0.8, curve: 0.0 });
        Clip { envelopes: vec![lane], gain_db: -6.0, ..Clip::new("c", Color(0), s, 2 * PPQ) }
    });
    let clip = &b.p.clips[&cid];
    let out = render_with(&b.p, &samples, (0, 4 * PPQ));
    let ch = out.channel(0);
    let gain = aitrack_dsp::db_to_gain(-6.0);
    for tick in [0, 100, 479, 480, 481, 700, 960, 1200, 1439, 1440, 1441, 1700, 1900] {
        let (got, want) = (ch[BEAT + SPT * tick as usize], gain * clip.gain_at(tick));
        assert!((got - want).abs() < 1e-4, "tick {tick}: {got} vs {want}");
    }
    // The curved segment really bends: halfway it is far below the chord.
    assert!(clip.gain_at(960) < 0.4);
    assert!(max_step(&ch[BEAT..3 * BEAT - 64]) < 1e-3);
}

// ───────────────────────────── Loops ─────────────────────────────

/// A clip over a 12 000-frame file (half a beat, 480 ticks) that starts
/// `offset` ticks in and is dragged out to `length` with
/// [`ops::resize_end`], which makes it loop from its offset to the file's end.
fn stretched(data: Vec<f32>, offset: Tick, length: Tick) -> (P, SampleCache, TrackId, ClipId) {
    assert_eq!(data.len(), 12_000);
    let (mut b, samples, t, cid) = one_clip(data, |s| Clip { offset, ..Clip::new("c", Color(0), s, 480 - offset) });
    let cmd = ops::resize_end(&b.p, cid, length).expect("resize");
    edit(&mut b.p, cmd);
    assert_eq!(b.p.clips[&cid].looping, Some(LoopRegion { start: offset, end: 480 }));
    (b, samples, t, cid)
}

#[test]
fn unwarped_loop_plays_whole_frames() {
    // Trimmed start: one pass of frames 2000..12 000, then the same again.
    let data = ramp(12_000);
    let (b, samples, ..) = stretched(data.clone(), 80, 4 * PPQ);
    let out = render_with(&b.p, &samples, (0, 6 * PPQ));
    let ch = out.channel(0);
    let len = 4 * BEAT;
    for n in 64..len - 64 {
        let m = n % 10_000;
        if m < 10_000 - 240 {
            assert!((ch[BEAT + n] - data[2000 + m]).abs() < 1e-6, "sample {n}: {} vs frame {}", ch[BEAT + n], 2000 + m);
        }
    }
    // Each wrap crossfades the tail (≈1.0) into the lead-up of the loop
    // start (frames 1760..2000): the 0.83 jump becomes 241 small steps.
    let jump = data[11_999] - data[1_999];
    for wrap in (10_000..len).step_by(10_000) {
        let seam = &ch[BEAT + wrap - 241..=BEAT + wrap];
        assert!(max_step(seam) < jump / 241.0 * 1.1, "wrap at {wrap}: step {}", max_step(seam));
        assert!((ch[BEAT + wrap - 1] - data[1_999]).abs() < 0.005 && (ch[BEAT + wrap] - data[2000]).abs() < 1e-6);
    }
    // It stops at the clip's length.
    assert!(peak(&ch[..BEAT]) == 0.0 && peak(&ch[BEAT + len + 1..]) == 0.0);

    // The whole file looped from its first frame: nothing leads up to the
    // loop start, so the tail fades into the first frame.
    let (b, samples, ..) = stretched(data.clone(), 0, 4 * PPQ);
    let out = render_with(&b.p, &samples, (0, 6 * PPQ));
    let ch = out.channel(0);
    for n in 0..len - 64 {
        let m = n % 12_000;
        if m < 12_000 - 240 {
            assert!((ch[BEAT + n] - data[m]).abs() < 1e-6, "sample {n}: {} vs frame {m}", ch[BEAT + n]);
        }
    }
    for wrap in (12_000..len).step_by(12_000) {
        let seam = &ch[BEAT + wrap - 241..=BEAT + wrap];
        assert!(max_step(seam) < 1.0 / 241.0 * 1.1, "wrap at {wrap}: step {}", max_step(seam));
    }
}

#[test]
fn loop_seam_is_no_rougher_than_the_source() {
    // A sine that does not loop cleanly: a hard wrap would jump by up to
    // twice its amplitude. With the seam the output never moves faster than
    // the sine itself does (10 % margin for the crossfade's own slope).
    for offset in [80, 0] {
        let data = sine(12_000);
        let own = max_step(&data);
        let (b, samples, ..) = stretched(data.clone(), offset, 4 * PPQ);
        let frames = 12_000 - offset as usize * SPT;
        let hard = (data[offset as usize * SPT] - data[11_999]).abs();
        assert!(hard > 4.0 * own, "the test loop must not be seamless by itself ({hard} vs {own})");
        let out = render_with(&b.p, &samples, (0, 6 * PPQ));
        let ch = out.channel(0);
        let played = &ch[BEAT + 64..5 * BEAT - 64];
        assert!(max_step(played) < own * 1.1, "offset {offset}: step {} vs the sine's {own}", max_step(played));
        // And the loop is there: mid-pass samples are the file's.
        for pass in 0..4 * BEAT / frames {
            let n = pass * frames + 3000;
            assert!((ch[BEAT + n] - data[offset as usize * SPT + 3000]).abs() < 1e-6, "pass {pass}");
        }
    }
}

#[test]
fn split_clip_renders_like_the_unsplit_one() {
    let check = |mut b: P, samples: SampleCache, t: TrackId, cut_tick: Tick, what: &str| {
        let whole = render_with(&b.p, &samples, (0, 6 * PPQ));
        let placement = b.p.track(t).unwrap().arrangement[0].id;
        let cmds = ops::split_at(&mut b.p, t, placement, PPQ + cut_tick).expect("split");
        edit(&mut b.p, Command::Batch { commands: cmds });
        assert_eq!(b.p.track(t).unwrap().arrangement.len(), 2);
        let halves = render_with(&b.p, &samples, (0, 6 * PPQ));
        let cut = BEAT + SPT * cut_tick as usize;
        assert!(peak(whole.channel(0)) > 0.3, "{what}: the clip is silent");
        for (i, (a, e)) in halves.channel(0).iter().zip(whole.channel(0)).enumerate() {
            // The cut edges have no fade, so each has its 64-sample ramp.
            if i + 64 < cut || i >= cut + 64 {
                assert!((a - e).abs() < 1e-4, "{what}: sample {i} ({} from the cut): {a} vs {e}", i as i64 - cut as i64);
            }
        }
        assert!(halves.channel(0)[cut].abs() < 1e-6, "{what}: the cut is ramped");
    };

    // Fades and a curved envelope segment that the cut divides.
    let (b, samples, t, _) = one_clip(noise(2 * BEAT), |s| {
        let mut lane = AutomationLane::new(AutomationTarget::Volume);
        lane.insert(AutomationPoint { tick: 0, value: 1.0, curve: 0.0 });
        lane.insert(AutomationPoint { tick: 600, value: 0.3, curve: 0.8 });
        lane.insert(AutomationPoint { tick: 1500, value: 0.9, curve: 0.0 });
        Clip {
            fade_in: Fade { length: 300, curve: 0.5 },
            fade_out: Fade { length: 400, curve: -0.3 },
            envelopes: vec![lane],
            ..Clip::new("c", Color(0), s, 2 * PPQ)
        }
    });
    check(b, samples, t, 777, "faded clip");

    // A trimmed, looping clip cut inside its third pass: the right half
    // starts inside the loop.
    let (mut b, samples, t, cid) = stretched(noise(12_000), 80, 4 * PPQ);
    let cmd = ops::set_fades(&b.p, cid, Fade { length: 200, curve: 0.0 }, Fade { length: 900, curve: 0.4 }).expect("fades");
    edit(&mut b.p, cmd);
    check(b, samples, t, 1100, "looping clip");
}

// ───────────────────────────── Session View ─────────────────────────────

/// A processor fed by hand, as the export does, but with the command queue
/// in reach.
struct Rig {
    proc_: AudioProcessor,
    cmd: rtrb::Producer<RtCommand>,
    _graph: rtrb::Producer<Box<crate::CompiledGraph>>,
    _retired: rtrb::Consumer<Box<crate::CompiledGraph>>,
}

impl Rig {
    fn new(p: &Project, samples: &SampleCache, playing: bool) -> Self {
        let mut pool = ProcessorPool::new();
        let graph = compile(
            p,
            &mut CompileContext {
                factory: &FallbackFactory,
                pool: &mut pool,
                samples,
                sample_rate: SR,
                max_frames: 256,
                prev_meters: None,
                taps: Vec::new(),
                analysis: None,
            },
        )
        .unwrap();
        assert!(graph.warnings().is_empty(), "{:?}", graph.warnings());
        let (mut graph_tx, graph_rx) = RingBuffer::new(2);
        let (retire_tx, retired) = RingBuffer::new(2);
        let (cmd, cmd_rx) = RingBuffer::new(16);
        let q = RtQueues { graph_rx, retire_tx, cmd_rx, midi_rx: Vec::new(), input_rx: None };
        let init = TransportInit { playing, ..TransportInit::default() };
        let mut proc_ = AudioProcessor::new(q, Arc::new(SharedStatus::new()), SR, 256, init);
        assert!(graph_tx.push(Box::new(graph)).is_ok());
        if !playing {
            // Install the graph before any command needs it.
            proc_.process_planar(&mut AudioBuffer::new(2, 256));
        }
        Self { proc_, cmd, _graph: graph_tx, _retired: retired }
    }

    /// The next `frames` samples of the left channel.
    fn run(&mut self, frames: usize) -> Vec<f32> {
        let mut buf = AudioBuffer::new(2, 256);
        let mut out = Vec::with_capacity(frames + 256);
        while out.len() < frames {
            self.proc_.process_planar(&mut buf);
            out.extend_from_slice(buf.channel(0));
        }
        out.truncate(frames);
        out
    }
}

/// A looping clip over `data` in the first scene, launched at tick 0.
fn launched(data: Vec<f32>, clip: impl FnOnce(SourceId) -> Clip, frames: usize) -> Vec<f32> {
    let mut b = P::new(120.0);
    let samples = SampleCache::blocking();
    let t = b.track(TrackKind::Audio, "audio");
    let sid = source(&mut b, &samples, "session", data, Warp::default());
    let cid = add_clip(&mut b, clip(sid));
    let scene = b.p.scenes[0].id;
    b.t(t).session_slots.insert(scene, cid);
    let mut rig = Rig::new(&b.p, &samples, false);
    assert!(rig.cmd.push(RtCommand::LaunchClip(t, scene)).is_ok());
    rig.run(frames)
}

#[test]
fn session_loop_fades_in_once_and_never_out() {
    let fade = Fade { length: 240, curve: 0.5 };
    let clip = |s| Clip { looping: Some(LoopRegion { start: 0, end: PPQ }), fade_in: fade, fade_out: fade, ..Clip::new("c", Color(0), s, PPQ) };
    let out = launched(vec![1.0; BEAT], clip, 5 * BEAT);
    let model = clip(SourceId(0));
    for tick in [0, 30, 120, 239] {
        let (got, want) = (out[SPT * tick as usize], model.gain_at(tick));
        assert!((got - want).abs() < 1e-4, "tick {tick}: {got} vs {want}");
    }
    assert!(out[0].abs() < 1e-4);
    // After the fade-in: unity through the end of the first pass (no
    // fade-out: the clip has no end), the seams and every later pass.
    let rest = &out[240 * SPT..];
    assert!(rest.iter().all(|s| (s - 1.0).abs() < 1e-4), "min {}", rest.iter().copied().fold(f32::MAX, f32::min));
}

#[test]
fn session_loop_crossfades_its_seam() {
    let data = sine(12_000);
    let own = max_step(&data);
    let clip = |s| Clip { looping: Some(LoopRegion { start: 0, end: 480 }), ..Clip::new("c", Color(0), s, 480) };
    let out = launched(data.clone(), clip, 5 * BEAT);
    assert!(max_step(&out) < own * 1.1, "step {} vs the sine's {own}", max_step(&out));
    for pass in 0..10 {
        assert!((out[pass * 12_000 + 3000] - data[3000]).abs() < 1e-6, "pass {pass}");
    }
}

// ───────────────────────────── Export ─────────────────────────────

#[test]
fn export_renders_a_faded_looped_clip() {
    // A real file on disk, read by the offline render's own sample cache.
    let path = tmp("faded-loop.wav");
    let data = ramp(12_000);
    let mut file = AudioBuffer::new(2, data.len());
    for ch in 0..2 {
        file.channel_mut(ch).copy_from_slice(&data);
    }
    write_wav(&path, &file, 48_000, WavFormat::Float32).unwrap();

    let mut b = P::new(120.0);
    let t = b.track(TrackKind::Audio, "audio");
    let sid = b.p.alloc_source_id();
    b.p.sources
        .insert(sid, Source::Audio(AudioSource { path, hash: None, sample_rate: 48_000, channels: 2, frames: 12_000, warp: Warp::default(), key: None }));
    let cid = add_clip(&mut b, Clip::new("c", Color(0), sid, 480));
    b.place(t, cid, 0);
    let cmd = ops::resize_end(&b.p, cid, 2 * PPQ).expect("resize");
    edit(&mut b.p, cmd);
    let cmd = ops::set_fades(&b.p, cid, Fade { length: 600, curve: 0.5 }, Fade { length: 300, curve: 0.0 }).expect("fades");
    edit(&mut b.p, cmd);
    let clip = &b.p.clips[&cid];
    assert!(clip.looping.is_some());

    let out = render_offline(&b.p, &FallbackFactory, (0, 3 * PPQ), SR, 256).unwrap();
    assert_eq!(out.frames(), 3 * BEAT);
    for ch in 0..2 {
        let got = out.channel(ch);
        // Four passes of the file, shaped by the fades.
        for tick in (0..1920).step_by(7) {
            let n = SPT * tick as usize;
            if n % 12_000 < 12_000 - 240 {
                let want = data[n % 12_000] * clip.gain_at(tick);
                assert!((got[n] - want).abs() < 1e-4, "tick {tick}: {} vs {want}", got[n]);
            }
        }
        assert!(got[0].abs() < 1e-6 && got[2 * BEAT - 1].abs() < 1e-3);
        assert!(peak(&got[2 * BEAT + 1..]) == 0.0);
        // The seams inside the faded clip are crossfaded.
        for wrap in [12_000, 24_000, 36_000] {
            assert!(max_step(&got[wrap - 241..=wrap]) < 1.0 / 241.0 * 1.1, "wrap at {wrap}");
        }
    }
}

// ───────────────────────────── Zero allocation ─────────────────────────────

#[test]
fn audio_clips_never_allocate() {
    let mut b = P::new(128.0);
    let samples = SampleCache::blocking();
    let shaped = |b: &mut P, sid: SourceId, offset: Tick, looping: (Tick, Tick)| {
        let mut lane = AutomationLane::new(AutomationTarget::Volume);
        for (i, value) in [1.0, 0.4, 0.9, 0.2, 1.0].into_iter().enumerate() {
            lane.insert(AutomationPoint { tick: i as Tick * 700, value, curve: [0.0, 0.8, -0.5][i % 3] });
        }
        let clip = Clip {
            offset,
            looping: Some(LoopRegion { start: looping.0, end: looping.1 }),
            fade_in: Fade { length: 900, curve: 0.7 },
            fade_out: Fade { length: 1200, curve: -0.4 },
            envelopes: vec![lane],
            ..Clip::new("c", Color(0), sid, 6 * PPQ)
        };
        add_clip(b, clip)
    };
    // Unwarped: whole-frame loop with a seam every 100 ticks.
    let plain = b.track(TrackKind::Audio, "plain");
    let sid = source(&mut b, &samples, "plain", sine(24_000), Warp::default());
    let c = shaped(&mut b, sid, 40, (20, 120));
    b.place(plain, c, 0);
    b.place(plain, c, 7 * PPQ);
    // Repitched and transposed: the interpolating path.
    let warped = b.track(TrackKind::Audio, "warped");
    let warp = Warp { enabled: true, original_bpm: Some(100.0), transpose: 3.0, ..Warp::default() };
    let sid = source(&mut b, &samples, "warped", sine(24_000), warp);
    let c = shaped(&mut b, sid, 0, (100, 300));
    b.place(warped, c, PPQ);
    // A plain faded one-shot.
    let one = add_clip(&mut b, Clip { fade_out: Fade { length: 400, curve: 0.3 }, ..Clip::new("o", Color(0), sid, PPQ) });
    b.place(warped, one, 8 * PPQ);
    // A looping clip launched in the Session View.
    let session = b.track(TrackKind::Audio, "session");
    let sid = source(&mut b, &samples, "session", sine(12_000), Warp::default());
    let c = shaped(&mut b, sid, 0, (0, 480));
    let scene = b.p.scenes[0].id;
    b.t(session).session_slots.insert(scene, c);

    let mut rig = Rig::new(&b.p, &samples, true);
    let mut buf = AudioBuffer::new(2, 256);
    rig.proc_.process_planar(&mut buf);
    assert!(rig.cmd.push(RtCommand::SetLoop(Some((0, 10 * PPQ)))).is_ok());
    assert!(rig.cmd.push(RtCommand::LaunchClip(session, scene)).is_ok());
    let mut loudest = 0.0f32;
    // 2500 blocks: more than two rounds of the 10-beat transport loop.
    let ((), allocs) = count_allocs(|| {
        for _ in 0..2500 {
            rig.proc_.process_planar(&mut buf);
            loudest = loudest.max(buf.peak(0));
        }
    });
    assert_eq!(allocs, 0, "clip playback with fades, envelopes and loops");
    assert!(loudest > 0.3, "the clips are audible ({loudest})");
    let g = rig.proc_.graph_mut().unwrap();
    assert!(g.tracks.iter().any(|t| t.session.playing.is_some()), "the session clip was launched");
}

// ───────────────────────────── Tempo changes ─────────────────────────────

#[test]
fn fades_follow_the_timeline_through_a_tempo_ramp() {
    // The tempo rises from 100 to 160 BPM under an unwarped clip. Its audio
    // keeps its speed, but the fades are drawn on the timeline: they must
    // sit on the clip's ticks and the fade-out must end where the clip does.
    let (mut b, samples, _, cid) = one_clip(vec![1.0; 6 * BEAT], |s| Clip {
        fade_in: Fade { length: 960, curve: 0.0 },
        fade_out: Fade { length: 1920, curve: 0.5 },
        ..Clip::new("c", Color(0), s, 4 * PPQ)
    });
    b.p.tempo.set_point(TempoPoint { start: 0, bpm: 100.0, ramp: true });
    b.p.tempo.set_point(TempoPoint { start: 6 * PPQ, bpm: 160.0, ramp: false });
    let clip = &b.p.clips[&cid];
    let out = render_with(&b.p, &samples, (0, 6 * PPQ));
    let ch = out.channel(0);
    let at = |tick: Tick| b.p.tempo.tick_to_samples(PPQ + tick, SR).round() as usize;
    for tick in [0, 200, 959, 960, 1500, 1920, 1921, 2500, 3200, 3800] {
        let (got, want) = (ch[at(tick)], clip.gain_at(tick));
        assert!((got - want).abs() < 1e-3, "tick {tick}: {got} vs {want}");
    }
    let end = at(4 * PPQ);
    assert!(ch[end - 2] > 0.0 && ch[end - 1].abs() < 1e-3, "last samples {} {}", ch[end - 2], ch[end - 1]);
    assert!(peak(&ch[end + 1..]) == 0.0);
}

// ───────────────────────────── Consolidate ─────────────────────────────

#[test]
fn consolidate_renders_the_clips_with_their_gain_and_the_gaps() {
    let mut b = P::new(120.0);
    let samples = SampleCache::blocking();
    let t = b.track(TrackKind::Audio, "audio");
    let data = ramp(2 * BEAT);
    let sid = source(&mut b, &samples, "joined", data.clone(), Warp::default());
    // Beat 1: the first beat of the file at half gain. Beat 3: its second beat.
    let quiet = add_clip(&mut b, Clip { gain_db: -6.020_6, ..Clip::new("a", Color(0), sid, PPQ) });
    let later = add_clip(&mut b, Clip { offset: PPQ, ..Clip::new("b", Color(0), sid, PPQ) });
    b.place(t, quiet, PPQ);
    b.place(t, later, 3 * PPQ);
    let unselected = add_clip(&mut b, Clip::new("c", Color(0), sid, PPQ));
    b.place(t, unselected, 2 * PPQ);
    let ids: Vec<PlacementId> = b.p.track(t).unwrap().arrangement.iter().filter(|pl| pl.clip != unselected).map(|pl| pl.id).collect();
    let out = render_clips(&b.p, t, &ids, (PPQ, 4 * PPQ), SR, Some(samples.clone())).unwrap();
    assert_eq!(out.frames(), 3 * BEAT);
    for ch in 0..2 {
        let c = out.channel(ch);
        // (The second clip starts inside the file: playback declicks its first samples.)
        for i in (500..BEAT).step_by(997) {
            assert!((c[i] - 0.5 * data[i]).abs() < 1e-4, "first clip at {i}: {} vs {}", c[i], 0.5 * data[i]);
            assert!((c[2 * BEAT + i] - data[BEAT + i]).abs() < 1e-4, "second clip at {i}");
        }
        assert_eq!(peak(&c[BEAT..2 * BEAT]), 0.0, "the gap is silent and the clip that was not chosen is left out");
    }

    // Written out: mono, because the file is mono, and warped at the song tempo.
    let dir = std::env::temp_dir().join(format!("aitrack-consolidate-{}", std::process::id()));
    let path = dir.join("joined.wav");
    let file = consolidate_to_wav(&b.p, t, &ids, (PPQ, 4 * PPQ), 48_000, &path, Some(samples)).unwrap();
    assert_eq!((file.channels, file.frames, file.sample_rate), (1, 3 * BEAT as u64, 48_000));
    assert_eq!((file.warp.enabled, file.warp.original_bpm), (true, Some(120.0)));
    assert_eq!(file.length_ticks(90.0), 3 * PPQ, "it stays three beats long at another tempo");
    let (decoded, rate) = crate::sample_cache::decode_file(&path).unwrap();
    assert_eq!((decoded.len(), decoded[0].len(), rate), (1, 3 * BEAT, 48_000));
    assert!((decoded[0][2 * BEAT + 600] - data[BEAT + 600]).abs() < 1e-4);
    let _ = std::fs::remove_dir_all(&dir);
    // Notes clips are refused.
    assert!(render_clips(&b.p, t, &[PlacementId(9999)], (0, PPQ), SR, None).is_err());
}
