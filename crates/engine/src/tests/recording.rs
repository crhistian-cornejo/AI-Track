//! Recording through the headless engine: a MIDI take with a count-in lands
//! its notes on the right ticks (1.4), a loop keeps capturing every pass,
//! audio input is written to a WAV aligned with the grid whatever the input
//! latency (1.5), monitoring follows In / Auto / Off, and none of it
//! allocates on the audio thread. Hardware latency is simulated by writing
//! the numbers the device callbacks would publish.

use crate::alloc_counter::count_allocs;
use crate::audio_io::ChannelSelection;
use crate::fallback::{FallbackFactory, TEST_SYNTH};
use crate::{AudioProcessor, Engine, EngineConfig, EngineHandle, MidiMessage};
use aitrack_dsp::AudioBuffer;
use aitrack_model::*;
use std::sync::Arc;

const SR: f64 = 48_000.0;
const BLOCK: usize = 256;
/// Samples per tick at 120 BPM and 48 kHz.
const SPT: usize = 25;
/// One 4/4 bar in samples and in ticks.
const BAR: usize = 96_000;
const BAR_TICKS: Tick = 3_840;

fn start(p: &Project) -> (EngineHandle, AudioProcessor) {
    let (h, proc_) = Engine::new(EngineConfig::headless(SR), Arc::new(FallbackFactory)).unwrap();
    h.update_project(p);
    h.flush();
    let mut proc_ = proc_.unwrap();
    let mut buf = AudioBuffer::new(2, BLOCK);
    proc_.process_planar(&mut buf);
    // The previous block's time span must be a positive time.
    std::thread::sleep(std::time::Duration::from_millis(20));
    (h, proc_)
}

fn synth_project() -> (Project, TrackId) {
    let mut p = Project::new("rec");
    let id = p.alloc_track_id();
    let mut t = Track::new(id, TrackKind::Instrument, "keys", Color(0));
    t.arm = true;
    let d = p.alloc_device_id();
    let mut dev = Device::stock(d, TEST_SYNTH, TEST_SYNTH);
    dev.params.insert(5, 0.5);
    t.devices.push(dev);
    p.tracks.push(t);
    (p, id)
}

/// A live message that happened `sample` frames into the span of the block
/// before the one about to run (what the audio thread reads in that block).
fn at(cb: u64, sample: usize) -> u64 {
    cb - (BLOCK as f64 / SR * 1e9) as u64 + (sample as f64 / SR * 1e9) as u64
}

/// One block with the given `(sample position since the transport's first
/// recorded frame, message)` pairs due in it. `first_block` is the index of
/// the block that starts at position 0, `m` the index of the block to run.
fn run_block(h: &EngineHandle, proc_: &mut AudioProcessor, m: usize, first_block: usize, events: &[(usize, MidiMessage)]) {
    proc_.begin_callback();
    let cb = proc_.callback_start_nanos();
    for (pos, msg) in events {
        // Played during block `k`, read in the next one.
        if first_block + pos / BLOCK + 1 == m {
            assert!(h.send_midi_at(*msg, at(cb, pos % BLOCK)));
        }
    }
    let mut buf = AudioBuffer::new(2, BLOCK);
    proc_.process_planar(&mut buf);
}

fn note(key: u8, start_tick: Tick, len_ticks: Tick) -> [(usize, MidiMessage); 2] {
    [(start_tick as usize * SPT, MidiMessage::note_on(0, key, 100)), ((start_tick + len_ticks) as usize * SPT, MidiMessage::note_off(0, key))]
}

#[test]
fn four_bars_after_a_count_in_land_their_notes_within_a_tick() {
    let (p, track) = synth_project();
    let (h, mut proc_) = start(&p);
    assert_eq!(h.recording_info().midi_tracks, vec![track]);
    h.set_count_in(1);
    h.set_recording(true);
    h.play();

    let mut events: Vec<(usize, MidiMessage)> = Vec::new();
    let planned: [(u8, Tick, Tick); 5] = [(60, 0, 480), (62, 960, 240), (64, 3_840, 960), (65, 7_777, 100), (67, 14_000, 1_000)];
    for (key, s, l) in planned {
        events.extend(note(key, s, l));
    }
    // A note held when the take stops, and one played during the count-in.
    events.push((15_000 * SPT, MidiMessage::note_on(0, 69, 90)));
    let count_in_blocks = BAR / BLOCK;
    assert_eq!(count_in_blocks * BLOCK, BAR);
    // The count-in note: block 100 of the count-in (positions of that span are negative).
    let take_blocks = 4 * BAR / BLOCK;
    let mut m = 1;
    // Block 0 was the warm-up; the count-in is blocks 1..=375, the take starts at block 376.
    let first_block = 1 + count_in_blocks;
    while m < first_block + take_blocks {
        if m == 100 {
            proc_.begin_callback();
            let cb = proc_.callback_start_nanos();
            h.send_midi_at(MidiMessage::note_on(0, 72, 100), at(cb, 40));
            let mut buf = AudioBuffer::new(2, BLOCK);
            proc_.process_planar(&mut buf);
        } else {
            run_block(&h, &mut proc_, m, first_block, &events);
        }
        m += 1;
        if m == first_block + 60 {
            let info = h.recording_info();
            assert!(info.capturing && info.armed, "{info:?}");
            assert_eq!(info.start_tick, 0.0, "the take starts at the position the count-in counted towards");
            assert!(h.finish_midi_recording().is_none(), "still capturing");
            // Periodic collection during the take: the first note is complete.
            let early = h.take_recorded_notes();
            assert_eq!(early.len(), 1, "{early:?}");
            assert_eq!((early[0].key, early[0].start_tick, early[0].length_ticks), (60, 0, 480));
        }
    }
    h.stop();
    for _ in 0..3 {
        run_block(&h, &mut proc_, 0, 0, &[]);
    }
    let info = h.recording_info();
    assert!(!info.capturing);
    assert_eq!(info.takes_started, info.takes_finished);
    assert_eq!(info.dropped, 0);

    let take = h.finish_midi_recording().expect("a finished take");
    assert_eq!((take.start_tick, take.end_tick, take.passes), (0, 4 * BAR_TICKS, 1));
    let by_key = |k: u8| take.notes.iter().find(|n| n.key == k).copied();
    // The first note was already handed out; the rest are within a tick of plan.
    assert!(by_key(60).is_none());
    for &(key, s, l) in &planned[1..] {
        let n = by_key(key).unwrap_or_else(|| panic!("key {key} missing: {:?}", take.notes));
        assert!((n.start_tick - s).abs() <= 1, "key {key}: start {} vs {s}", n.start_tick);
        assert!((n.length_ticks - l).abs() <= 1, "key {key}: length {} vs {l}", n.length_ticks);
        assert_eq!((n.track, n.pass), (track, 0));
        assert!((n.velocity - 100.0 / 127.0).abs() < 1e-6);
    }
    let held = by_key(69).expect("the held note is closed at the end of the take");
    assert!((held.start_tick - 15_000).abs() <= 1 && (held.start_tick + held.length_ticks - 4 * BAR_TICKS).abs() <= 1, "{held:?}");
    assert!(by_key(72).is_none(), "nothing played during the count-in is recorded");
    assert_eq!(take.notes.len(), 5, "{:?}", take.notes);
    assert!(h.finish_midi_recording().is_none(), "a take is handed out once");
}

#[test]
fn every_loop_pass_keeps_capturing() {
    let (p, track) = synth_project();
    let (h, mut proc_) = start(&p);
    h.set_loop(Some((0, BAR_TICKS)));
    h.set_recording(true);
    h.play();
    let mut events: Vec<(usize, MidiMessage)> = Vec::new();
    // Pass 0, 1 and 2 each play a note at the same place; pass 0 also has
    // one that ends right before the wrap (read after it).
    for pass in 0..3usize {
        for e in note(60 + pass as u8, 960, 480) {
            events.push((pass * BAR + e.0, e.1));
        }
    }
    for e in note(70, 3_830, 5) {
        events.push(e);
    }
    // Three passes, stopped one block before the third wrap (which would open an empty fourth).
    let blocks = 3 * BAR / BLOCK - 1;
    for m in 1..=blocks {
        run_block(&h, &mut proc_, m, 1, &events);
        if m == BAR / BLOCK + 3 {
            assert_eq!(h.recording_info().pass, 1);
        }
    }
    h.stop();
    for _ in 0..3 {
        run_block(&h, &mut proc_, 0, 0, &[]);
    }
    let take = h.finish_midi_recording().expect("take");
    assert_eq!(take.passes, 3);
    for pass in 0..3u32 {
        let n = take.notes.iter().find(|n| n.key == 60 + pass as u8).copied().unwrap_or_else(|| panic!("pass {pass}: {:?}", take.notes));
        assert_eq!((n.pass, n.track), (pass, track));
        assert!((n.start_tick - 960).abs() <= 1 && (n.length_ticks - 480).abs() <= 1, "{n:?}");
    }
    let edge = take.notes.iter().find(|n| n.key == 70).copied().expect("the note before the wrap");
    assert_eq!(edge.pass, 0, "played before the wrap, read after it");
    assert!((edge.start_tick - 3_830).abs() <= 1, "{edge:?}");
}

#[test]
fn an_unarmed_track_records_nothing() {
    let (mut p, track) = synth_project();
    p.track_mut(track).unwrap().arm = false;
    let (h, mut proc_) = start(&p);
    h.set_midi_target(Some(track));
    h.set_recording(true);
    h.play();
    let events: Vec<_> = note(60, 480, 480).to_vec();
    for m in 1..=40 {
        run_block(&h, &mut proc_, m, 1, &events);
    }
    h.stop();
    run_block(&h, &mut proc_, 0, 0, &[]);
    let take = h.finish_midi_recording().expect("take");
    assert!(take.notes.is_empty());
    assert!(h.recording_info().midi_tracks.is_empty());
}

// ───────────────────────────── Audio ─────────────────────────────

fn audio_project() -> (Project, TrackId) {
    let mut p = Project::new("audio rec");
    let id = p.alloc_track_id();
    let mut t = Track::new(id, TrackKind::Audio, "mic", Color(0));
    t.arm = true;
    t.monitor = Monitor::Off;
    p.tracks.push(t);
    (p, id)
}

/// Feed `BLOCK` stereo frames of the ramp `frame / 1e6` (left) and `-frame / 1e6` (right).
fn feed_ramp(h: &EngineHandle, counter: &mut usize) {
    let mut frames = Vec::with_capacity(2 * BLOCK);
    for _ in 0..BLOCK {
        let v = *counter as f32 / 1e6;
        frames.push(v);
        frames.push(-v);
        *counter += 1;
    }
    assert_eq!(h.feed_input(&frames), 2 * BLOCK);
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("aitrack-rec-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn wait_for_files(h: &EngineHandle, n: usize) -> Vec<crate::record::AudioTake> {
    let mut all = Vec::new();
    for _ in 0..200 {
        all.extend(h.finish_audio_recording());
        if all.len() >= n {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    all
}

#[test]
fn audio_lands_on_the_grid_whatever_the_input_latency() {
    let (p, track) = audio_project();
    let (h, mut proc_) = start(&p);
    let dir = temp_dir("align");
    h.set_recording_dir(&dir);
    std::thread::sleep(std::time::Duration::from_millis(30));
    // The device callbacks would publish these: 100 frames from the
    // microphone to the callback, 50 from the callback to the speakers.
    h.test_status().io.publish_input(64, 100);
    h.test_status().io.publish_output(32, 50);
    let comp = 150usize;

    let mut counter = 0usize;
    let mut buf = AudioBuffer::new(2, BLOCK);
    feed_ramp(&h, &mut counter);
    proc_.process_planar(&mut buf); // (installs nothing new; one block of input before the take)
    h.set_recording(true);
    h.play();
    let take_blocks = 40;
    for _ in 0..take_blocks {
        feed_ramp(&h, &mut counter);
        proc_.process_planar(&mut buf);
    }
    h.stop();
    for _ in 0..3 {
        feed_ramp(&h, &mut counter);
        proc_.process_planar(&mut buf);
    }
    let takes = wait_for_files(&h, 1);
    assert_eq!(takes.len(), 1, "{takes:?}");
    let t = &takes[0];
    assert_eq!((t.track, t.channels, t.sample_rate, t.error.clone()), (track, 2, 48_000, None));
    assert_eq!(t.start_tick, 0.0, "the first frame sits on the take's start tick");
    assert_eq!(t.frames as usize, take_blocks * BLOCK, "as long as the take: the head latency is dropped, the tail kept");
    assert!(t.path.starts_with(&dir));
    let mut r = hound::WavReader::open(&t.path).expect("a WAV");
    assert_eq!((r.spec().channels, r.spec().sample_rate), (2, 48_000));
    let samples: Vec<f32> = r.samples::<f32>().map(|s| s.unwrap()).collect();
    assert_eq!(samples.len(), 2 * take_blocks * BLOCK);
    // File frame k is the input frame that was `comp` frames after the take's
    // first block: the warm-up block of input (BLOCK frames) precedes it.
    for k in [0usize, 1, 100, 255, 256, 5_000, take_blocks * BLOCK - 1] {
        let expect = (BLOCK + comp + k) as f32 / 1e6;
        assert!((samples[2 * k] - expect).abs() < 1e-9, "left {k}: {} vs {expect}", samples[2 * k]);
        assert!((samples[2 * k + 1] + expect).abs() < 1e-9, "right {k}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_track_records_the_channels_it_picked() {
    let (p, track) = audio_project();
    let (h, mut proc_) = start(&p);
    let dir = temp_dir("pick");
    h.set_recording_dir(&dir);
    std::thread::sleep(std::time::Duration::from_millis(30));
    h.set_track_input(track, Some(ChannelSelection { first: 1, count: 1 }));
    h.set_recording(true);
    h.play();
    let mut counter = 0usize;
    let mut buf = AudioBuffer::new(2, BLOCK);
    for _ in 0..8 {
        feed_ramp(&h, &mut counter);
        proc_.process_planar(&mut buf);
    }
    h.stop();
    feed_ramp(&h, &mut counter);
    proc_.process_planar(&mut buf);
    let t = wait_for_files(&h, 1).pop().expect("a file");
    assert_eq!(t.channels, 1, "one channel picked");
    let mut r = hound::WavReader::open(&t.path).unwrap();
    let s: Vec<f32> = r.samples::<f32>().map(|s| s.unwrap()).collect();
    assert_eq!(s[10], -(10.0f32 / 1e6), "the right channel of the device, frame 10");
    let _ = std::fs::remove_dir_all(&dir);
}

fn dc_input(h: &EngineHandle, l: f32, r: f32) {
    let frames: Vec<f32> = (0..BLOCK).flat_map(|_| [l, r]).collect();
    assert_eq!(h.feed_input(&frames), 2 * BLOCK);
}

fn level(proc_: &mut AudioProcessor, h: &EngineHandle, l: f32, r: f32) -> (f32, f32) {
    dc_input(h, l, r);
    let mut buf = AudioBuffer::new(2, BLOCK);
    proc_.process_planar(&mut buf);
    (buf.channel(0)[BLOCK - 1], buf.channel(1)[BLOCK - 1])
}

#[test]
fn monitoring_follows_in_auto_and_off() {
    let (p, track) = audio_project();
    let (h, mut proc_) = start(&p);
    let near = |a: f32, b: f32| (a - b).abs() < 1e-3;
    level(&mut proc_, &h, 0.0, 0.0);

    // Off (the project's mode): silent.
    assert_eq!(level(&mut proc_, &h, 0.5, 0.25), (0.0, 0.0));
    // In: heard while armed, whatever the transport does.
    h.set_track_monitor(track, Some(Monitor::In));
    let (l, r) = level(&mut proc_, &h, 0.5, 0.25);
    assert!(near(l, 0.5) && near(r, 0.25), "{l} {r}");
    h.play();
    let (l, _) = level(&mut proc_, &h, 0.5, 0.25);
    assert!(near(l, 0.5), "In while playing: {l}");
    // Auto: heard when stopped or recording, not when playing back.
    h.set_track_monitor(track, Some(Monitor::Auto));
    let (l, _) = level(&mut proc_, &h, 0.5, 0.25);
    assert_eq!(l, 0.0, "Auto while the transport plays back");
    h.set_recording(true);
    let (l, _) = level(&mut proc_, &h, 0.5, 0.25);
    assert!(near(l, 0.5), "Auto while recording: {l}");
    h.set_recording(false);
    h.stop();
    let (l, _) = level(&mut proc_, &h, 0.5, 0.25);
    assert!(near(l, 0.5), "Auto when stopped: {l}");
    // The track's own channel pick feeds the monitor too.
    h.set_track_input(track, Some(ChannelSelection { first: 1, count: 1 }));
    let (l, r) = level(&mut proc_, &h, 0.5, 0.25);
    assert!(near(l, 0.25) && near(r, 0.25), "mono from device channel 2: {l} {r}");
    h.set_track_monitor(track, Some(Monitor::Off));
    assert_eq!(level(&mut proc_, &h, 0.5, 0.25), (0.0, 0.0));
}

#[test]
fn recording_midi_and_audio_never_allocates_on_the_audio_thread() {
    let (mut p, keys) = synth_project();
    let mic = p.alloc_track_id();
    let mut t = Track::new(mic, TrackKind::Audio, "mic", Color(0));
    t.arm = true;
    t.monitor = Monitor::In;
    p.tracks.push(t);
    let (h, mut proc_) = start(&p);
    let dir = temp_dir("alloc");
    h.set_recording_dir(&dir);
    std::thread::sleep(std::time::Duration::from_millis(30));
    h.set_loop(Some((0, 2 * PPQ)));
    h.set_count_in(1);
    h.set_recording(true);
    h.play();
    let mut buf = AudioBuffer::new(2, BLOCK);
    let _ = keys;
    let input = [0.1f32; 2 * BLOCK];
    let ((), allocs) = count_allocs(|| {
        for i in 0..2_000usize {
            proc_.begin_callback();
            let cb = proc_.callback_start_nanos();
            let key = 60 + (i % 12) as u8;
            h.send_midi_at(MidiMessage::note_on(0, key, 90), at(cb, 17));
            h.send_midi_at(MidiMessage::note_off(0, key), at(cb, 200));
            h.feed_input(&input);
            proc_.process_planar(&mut buf);
        }
    });
    assert_eq!(allocs, 0, "count-in, loop wraps, MIDI capture and audio chunks included");
    let info = h.recording_info();
    assert!(info.capturing, "{info:?}");
    assert!(info.pass >= 1, "the loop wrapped while recording: {info:?}");
    h.stop();
    run_block(&h, &mut proc_, 0, 0, &[]);
    let take = h.finish_midi_recording().expect("take");
    assert!(take.notes.len() > 100, "{}", take.notes.len());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_note_held_across_the_loop_wrap_ends_at_the_loop_end() {
    let (p, track) = synth_project();
    let (h, mut proc_) = start(&p);
    h.set_loop(Some((0, BAR_TICKS)));
    h.set_recording(true);
    h.play();
    let key = |k: u8| (k, 90);
    let _ = key;
    let mut events: Vec<(usize, MidiMessage)> = vec![
        // Pressed at tick 3000 of pass 0, released at tick 500 of pass 1.
        (3_000 * SPT, MidiMessage::note_on(0, 60, 100)),
        (BAR + 500 * SPT, MidiMessage::note_off(0, 60)),
        // Pressed at tick 3600 of pass 1 and still held when the take stops in pass 2.
        (BAR + 3_600 * SPT, MidiMessage::note_on(0, 62, 100)),
    ];
    events.sort_by_key(|e| e.0);
    // Two passes and 10 blocks of the third (about tick 102).
    for m in 1..=(2 * BAR / BLOCK + 10) {
        run_block(&h, &mut proc_, m, 1, &events);
    }
    h.stop();
    for _ in 0..3 {
        run_block(&h, &mut proc_, 0, 0, &[]);
    }
    let take = h.finish_midi_recording().expect("take");
    let mut notes: Vec<_> = take.notes.iter().map(|n| (n.key, n.pass, n.start_tick, n.length_ticks, n.track)).collect();
    notes.sort();
    let near = |a: Tick, b: Tick| (a - b).abs() <= 1;
    // What sounded: the synth is silenced at the wrap, so a held key ends at
    // the loop end and nothing starts again in the next pass.
    assert_eq!(notes.len(), 2, "{notes:?}");
    let [a0, b1] = [notes[0], notes[1]];
    assert_eq!((a0.0, a0.1, a0.4), (60, 0, track));
    assert!(near(a0.2, 3_000) && near(a0.3, BAR_TICKS - 3_000), "ends at the loop end: {a0:?}");
    assert_eq!((b1.0, b1.1), (62, 1));
    assert!(near(b1.2, 3_600) && near(b1.3, BAR_TICKS - 3_600), "held note of the last full pass ends at the loop end: {b1:?}");
}

#[test]
fn a_track_with_no_input_neither_records_nor_monitors() {
    let (mut p, track) = audio_project();
    p.track_mut(track).unwrap().monitor = Monitor::In;
    let (h, mut proc_) = start(&p);
    let dir = temp_dir("noinput");
    h.set_recording_dir(&dir);
    std::thread::sleep(std::time::Duration::from_millis(30));
    assert_eq!(h.recording_info().audio_tracks, vec![track]);
    let near = |a: f32, b: f32| (a - b).abs() < 1e-3;
    assert!(near(level(&mut proc_, &h, 0.5, 0.25).0, 0.5), "monitored while it has an input");

    h.set_track_input_none(track, true);
    h.flush();
    assert!(h.recording_info().audio_tracks.is_empty(), "not listed as recording");
    assert_eq!(level(&mut proc_, &h, 0.5, 0.25), (0.0, 0.0), "not monitored");
    h.set_recording(true);
    h.play();
    let mut counter = 0usize;
    let mut buf = AudioBuffer::new(2, BLOCK);
    for _ in 0..8 {
        feed_ramp(&h, &mut counter);
        proc_.process_planar(&mut buf);
    }
    h.stop();
    for _ in 0..3 {
        feed_ramp(&h, &mut counter);
        proc_.process_planar(&mut buf);
    }
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert!(h.finish_audio_recording().is_empty(), "no WAV for a track without input");
    assert!(!dir.exists() || std::fs::read_dir(&dir).unwrap().next().is_none());

    // Back to an input: it records again.
    h.set_track_input_none(track, false);
    h.flush();
    assert_eq!(h.recording_info().audio_tracks, vec![track]);
    h.set_recording(true);
    h.play();
    for _ in 0..4 {
        feed_ramp(&h, &mut counter);
        proc_.process_planar(&mut buf);
    }
    h.stop();
    feed_ramp(&h, &mut counter);
    proc_.process_planar(&mut buf);
    assert_eq!(wait_for_files(&h, 1).len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

/// One note (on at frame `on`, off at frame `off` of the previous block's
/// span), played in the next block; returns the tick the previous block
/// ended at.
fn play_note(h: &EngineHandle, proc_: &mut AudioProcessor, key: u8, on: usize, off: usize) -> f64 {
    let t0 = h.position_ticks();
    proc_.begin_callback();
    let cb = proc_.callback_start_nanos();
    assert!(h.send_midi_at(MidiMessage::note_on(0, key, 100), at(cb, on)));
    assert!(h.send_midi_at(MidiMessage::note_off(0, key), at(cb, off)));
    let mut buf = AudioBuffer::new(2, BLOCK);
    proc_.process_planar(&mut buf);
    t0
}

#[test]
fn a_tempo_change_during_a_take_keeps_recording_on_the_right_ticks() {
    let (mut p, track) = synth_project();
    let (h, mut proc_) = start(&p);
    // From bar 2, at 120 BPM.
    h.seek(BAR_TICKS);
    h.set_recording(true);
    h.play();
    let mut buf = AudioBuffer::new(2, BLOCK);
    for _ in 0..20 {
        proc_.process_planar(&mut buf);
    }
    // Faster, then slower than the start: a tick is 12.5, then 50 samples.
    for (bpm, samples_per_tick, key) in [(240.0, 12.5f64, 60u8), (60.0, 50.0, 62)] {
        p.tempo = TempoMap::constant(bpm);
        h.update_project(&p);
        h.flush();
        proc_.process_planar(&mut buf);
        proc_.process_planar(&mut buf);
        let t0 = play_note(&h, &mut proc_, key, 100, 200);
        // The note was played in the previous block's span: frames 100 and 200 of it.
        let _ = track;
        let want_on = t0 + (100.0 - BLOCK as f64) / samples_per_tick;
        let want_len = 100.0 / samples_per_tick;
        proc_.process_planar(&mut buf);
        let mut got = None;
        for n in h.take_recorded_notes() {
            if n.key == key {
                got = Some(n);
            }
        }
        let n = got.unwrap_or_else(|| panic!("no note recorded at {bpm} BPM"));
        // (positions are rounded to whole ticks; at 240 BPM a tick is 12.5 samples)
        assert!((n.start_tick as f64 - want_on).abs() <= 1.0, "{bpm} BPM: start {} vs {want_on}", n.start_tick);
        assert!((n.length_ticks as f64 - want_len).abs() <= 1.0, "{bpm} BPM: length {} vs {want_len}", n.length_ticks);
    }
}

// ───────────────────────────── Reopening, full queues, queue wait ─────────────────────────────

fn wav_frames(path: &std::path::Path) -> usize {
    let r = hound::WavReader::open(path).expect("a finished WAV");
    r.len() as usize / r.spec().channels as usize
}

#[test]
fn reopening_the_device_mid_take_ends_the_take_and_keeps_every_file() {
    let (mut p, keys) = synth_project();
    let mic = p.alloc_track_id();
    let mut t = Track::new(mic, TrackKind::Audio, "mic", Color(0));
    t.arm = true;
    t.monitor = Monitor::Off;
    p.tracks.push(t);
    let (h, mut proc_) = start(&p);
    let dir = temp_dir("reopen");
    h.set_recording_dir(&dir);
    std::thread::sleep(std::time::Duration::from_millis(30));
    h.set_recording(true);
    h.play();
    let mut counter = 0usize;
    // A key pressed in the first moments and never released.
    let events = [(300usize, MidiMessage::note_on(0, 60, 100))];
    for m in 1..=20 {
        feed_ramp(&h, &mut counter);
        run_block(&h, &mut proc_, m, 1, &events);
    }
    assert!(h.recording_info().capturing);
    // The device is reopened (headphones plugged in...): a new audio thread.
    let mut proc2 = h.test_rebuild();
    drop(proc_);
    let info = h.recording_info();
    assert!(!info.capturing);
    assert_eq!(info.takes_started, 1);
    assert_eq!(info.takes_finished, 1, "the take in progress was closed for the app to collect");
    let take = h.finish_midi_recording().expect("the take is delivered");
    assert_eq!(take.notes.len(), 1, "{:?}", take.notes);
    assert!(take.notes[0].length_ticks > 1, "the held key ends where the take was cut: {:?}", take.notes[0]);
    let first = wait_for_files(&h, 1);
    assert_eq!(first.len(), 1, "{first:?}");
    assert_eq!(first[0].error, None);
    let first_frames = wav_frames(&first[0].path);
    assert!(first_frames > 10 * BLOCK, "{first_frames}");

    // The next take starts normally and never touches the first file.
    h.set_recording(true);
    h.play();
    let mut buf = AudioBuffer::new(2, BLOCK);
    for _ in 0..10 {
        feed_ramp(&h, &mut counter);
        proc2.process_planar(&mut buf);
    }
    h.stop();
    feed_ramp(&h, &mut counter);
    proc2.process_planar(&mut buf);
    let second = wait_for_files(&h, 1);
    assert_eq!(second.len(), 1, "{second:?}");
    assert_ne!(second[0].path, first[0].path, "a new name for every take");
    assert_eq!(wav_frames(&first[0].path), first_frames, "the first file is untouched");
    assert_eq!(h.recording_info().takes_started, 2);
    assert!(h.finish_midi_recording().is_some(), "and its MIDI take is delivered too");
    let _ = (keys, std::fs::remove_dir_all(&dir));
}

#[test]
fn audio_alignment_follows_how_long_the_input_waited_in_the_queue() {
    let (p, track) = audio_project();
    let (h, mut proc_) = start(&p);
    let dir = temp_dir("queue");
    h.set_recording_dir(&dir);
    std::thread::sleep(std::time::Duration::from_millis(60));
    h.test_status().io.publish_output(32, 50);
    let block_ns = (BLOCK as f64 / SR * 1e9) as u64;
    let mut counter = 0usize;
    let mut buf = AudioBuffer::new(2, BLOCK);
    let mut paths = Vec::new();
    // The input was captured 0, 1, 2.5 and 4 blocks before the engine read it.
    for (take, wait_blocks) in [0.0f64, 1.0, 2.5, 4.0].into_iter().enumerate() {
        let waited = (wait_blocks * BLOCK as f64) as usize;
        let waited_ns = (waited as f64 / SR * 1e9) as u64;
        h.set_recording(true);
        h.play();
        let (first_frame, mut stamp) = (counter, 0u64);
        for b in 0..20 {
            proc_.begin_callback();
            let cb = proc_.callback_start_nanos();
            stamp = if b == 0 { cb - waited_ns } else { stamp + block_ns };
            let frames: Vec<f32> = (0..BLOCK).flat_map(|i| [(counter + i) as f32 / 1e6, -((counter + i) as f32) / 1e6]).collect();
            counter += BLOCK;
            assert_eq!(h.feed_input_at(&frames, stamp), 2 * BLOCK);
            proc_.process_planar(&mut buf);
        }
        h.stop();
        // (The tail still on its way through the queue takes as many blocks as the wait.)
        for _ in 0..6 {
            proc_.begin_callback();
            stamp += block_ns;
            let frames: Vec<f32> = (0..BLOCK).flat_map(|i| [(counter + i) as f32 / 1e6, -((counter + i) as f32) / 1e6]).collect();
            counter += BLOCK;
            h.feed_input_at(&frames, stamp);
            proc_.process_planar(&mut buf);
        }
        let t = wait_for_files(&h, 1).pop().unwrap_or_else(|| panic!("take {take}: no file"));
        assert_eq!(t.track, track);
        let mut r = hound::WavReader::open(&t.path).unwrap();
        let s: Vec<f32> = r.samples::<f32>().map(|s| s.unwrap()).collect();
        // The file starts on the take's start tick: the sample captured
        // `waited + output latency` frames after the first one the take read.
        let expect = (first_frame + waited + 50) as f32 / 1e6;
        assert!((s[0] - expect).abs() < 1e-9, "take {take} (waited {wait_blocks} blocks): first sample {} vs {expect}", s[0]);
        assert_eq!(t.start_tick, 0.0);
        paths.push(t.path);
    }
    paths.sort();
    paths.dedup();
    assert_eq!(paths.len(), 4, "every take has its own file");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn lost_and_late_input_frames_are_counted() {
    let (p, _) = audio_project();
    let (h, mut proc_) = start(&p);
    let before = h.recording_info();
    let mut buf = AudioBuffer::new(2, BLOCK);
    // No input at all for a block: silence made up for it.
    proc_.process_planar(&mut buf);
    assert_eq!(h.recording_info().input_underrun_frames, before.input_underrun_frames + BLOCK as u64);
    // A backlog of twenty blocks: whole frames beyond four blocks are thrown away.
    let mut counter = 0usize;
    for _ in 0..20 {
        feed_ramp(&h, &mut counter);
    }
    proc_.process_planar(&mut buf);
    let lost = h.recording_info().input_dropped_frames;
    assert_eq!(lost, ((20 - 1 - 4) * BLOCK) as u64, "{lost}");
}
