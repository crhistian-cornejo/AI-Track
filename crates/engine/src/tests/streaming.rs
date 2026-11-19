//! Disk streaming through the whole engine: a long file played from disk
//! sounds bit-identical to the same file in RAM (offline and live, plain,
//! looping, trimmed, reversed, transposed, warped to the tempo, after a
//! seek, on one thread and on many), a page that comes too late is silence
//! and is counted, and neither the audio thread nor the workers allocate
//! while clips stream.

use super::*;
use crate::alloc_counter::{count_allocs_in_pools, WORKER_COUNT_LOCK};
use crate::stream::{StreamSettings, PAGE};
use crate::{AudioProcessor, EngineHandle};
use std::path::{Path, PathBuf};
use std::time::Duration;

const BLOCK: usize = 256;
/// Every file streams, however short.
const ALL: StreamSettings = StreamSettings { enabled: true, min_seconds: 0, min_megabytes: 0 };

/// A stereo 24-bit WAV of `seconds` at 48 kHz: two detuned partials and a
/// little seeded noise, different on each side, so any misplaced frame shows.
fn long_wav(name: &str, seconds: f64) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("aitrack-streaming-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    // Tests share the file: one writes it (whole, then renamed into place).
    static WRITING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _one = WRITING.lock().unwrap_or_else(|p| p.into_inner());
    if path.exists() {
        return path;
    }
    let part = path.with_extension("part");
    let spec = hound::WavSpec { channels: 2, sample_rate: 48_000, bits_per_sample: 24, sample_format: hound::SampleFormat::Int };
    let mut w = hound::WavWriter::create(&part, spec).unwrap();
    let mut x = 0x2468_ace1u32;
    let frames = (seconds * SR) as usize;
    for i in 0..frames {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        let t = i as f64 / SR;
        let n = (x as f64 / u32::MAX as f64 - 0.5) * 0.05;
        let l = 0.4 * (t * 220.0 * std::f64::consts::TAU).sin() + 0.2 * (t * 3301.0 * std::f64::consts::TAU).sin() + n;
        let r = 0.4 * (t * 277.0 * std::f64::consts::TAU).sin() - 0.2 * (t * 5003.0 * std::f64::consts::TAU).sin() - n;
        w.write_sample((l * 8_000_000.0) as i32).unwrap();
        w.write_sample((r * 8_000_000.0) as i32).unwrap();
    }
    w.finalize().unwrap();
    std::fs::rename(&part, &path).unwrap();
    path
}

fn source(p: &mut Project, path: &Path, warp: Warp) -> SourceId {
    let frames = hound::WavReader::open(path).unwrap().duration() as u64;
    let sid = p.alloc_source_id();
    p.sources.insert(sid, Source::Audio(AudioSource { path: path.to_path_buf(), hash: None, sample_rate: 48_000, channels: 2, frames, warp, key: None }));
    sid
}

/// One track per way of reading a file: plain from the start, trimmed
/// and looping (the seam reads the lead-up), reversed and transposed
/// (interpolated, read backwards), warped to the project tempo in Re-Pitch.
/// 120 BPM: a beat is half a second.
fn reads(path: &Path) -> (Project, Vec<TrackId>) {
    let mut b = P::new(120.0);
    let mut ids = Vec::new();
    let mut add = |b: &mut P, name: &str, warp: Warp, clip: &dyn Fn(SourceId) -> Clip, at: Tick| {
        let t = b.track(TrackKind::Audio, name);
        let sid = source(&mut b.p, path, warp);
        let cid = b.p.alloc_clip_id();
        b.p.clips.insert(cid, clip(sid));
        b.place(t, cid, at);
        ids.push(t);
    };
    add(&mut b, "plain", Warp::default(), &|sid| Clip::new("plain", Color(0), sid, 12 * PPQ), 0);
    add(
        &mut b,
        "looped",
        Warp::default(),
        &|sid| Clip { offset: PPQ / 3, looping: Some(LoopRegion { start: 2 * PPQ, end: 5 * PPQ }), gain_db: -3.0, ..Clip::new("looped", Color(0), sid, 12 * PPQ) },
        PPQ / 2,
    );
    add(&mut b, "reversed", Warp { reverse: true, transpose: 3.0, ..Warp::default() }, &|sid| Clip::new("rev", Color(0), sid, 10 * PPQ), PPQ);
    add(
        &mut b,
        "warped",
        Warp { enabled: true, original_bpm: Some(100.0), mode: WarpMode::Repitch, ..Warp::default() },
        &|sid| Clip { offset: 3 * PPQ, ..Clip::new("warped", Color(0), sid, 9 * PPQ) },
        0,
    );
    (b.p, ids)
}

fn render_with(p: &Project, samples: &SampleCache, threads: usize, stems: &[TrackId]) -> Rendered {
    let opts = RenderOptions { range: RenderRange::Ticks(0, 14 * PPQ), block: BLOCK, tail_seconds: 0.5, samples: Some(samples.clone()), threads: Some(threads), ..RenderOptions::default() };
    crate::export::render(p, &FallbackFactory, &opts, stems, None).unwrap()
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

fn cache(settings: StreamSettings) -> SampleCache {
    let c = SampleCache::blocking();
    c.set_streaming(settings);
    c
}

#[test]
fn a_long_file_renders_the_same_from_disk_as_from_ram() {
    let path = long_wav("reads.wav", 9.0);
    let (p, ids) = reads(&path);
    let ram = cache(StreamSettings::OFF);
    let disk = cache(ALL);
    let a = render_with(&p, &ram, 1, &ids);
    assert_eq!(ram.streamed_files(), 0);
    let b = render_with(&p, &disk, 1, &ids);
    assert_eq!(disk.streamed_files(), 1, "the file streamed");
    assert_eq!(disk.used_bytes(), 0, "and takes no cache memory");
    assert!(peak(a.mix.channel(0)) > 0.1);
    same_bits(&a.mix, &b.mix, "mix");
    for (x, y) in a.stems.iter().zip(&b.stems) {
        assert!(peak(x.2.channel(0)) > 0.01, "'{}' plays", x.1);
        same_bits(&x.2, &y.2, &format!("stem '{}'", x.1));
    }
    // The default settings keep a 9 s file in RAM.
    let default = cache(StreamSettings::default());
    same_bits(&a.mix, &render_with(&p, &default, 1, &[]).mix, "default settings");
    assert_eq!(default.streamed_files(), 0);
}

/// A file that cannot be read in place (another rate) streams from its
/// converted copy, still bit-identical, and a pitch-preserving clip of a
/// streamed file still gets its stretched render.
#[test]
fn converted_and_stretched_files_render_the_same_from_disk() {
    let dir = std::env::temp_dir().join(format!("aitrack-streaming-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("rate-44k.wav");
    let spec = hound::WavSpec { channels: 1, sample_rate: 44_100, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut w = hound::WavWriter::create(&path, spec).unwrap();
    for i in 0..44_100 * 4 {
        w.write_sample(((i as f32 * 0.031).sin() * 20_000.0) as i16).unwrap();
    }
    w.finalize().unwrap();
    let mut b = P::new(120.0);
    let t = b.track(TrackKind::Audio, "conv");
    let sid = source(&mut b.p, &path, Warp::default());
    let cid = b.p.alloc_clip_id();
    b.p.clips.insert(cid, Clip { looping: Some(LoopRegion { start: PPQ, end: 3 * PPQ }), ..Clip::new("c", Color(0), sid, 10 * PPQ) });
    b.place(t, cid, 0);
    let s = b.track(TrackKind::Audio, "stretched");
    let sid = source(&mut b.p, &path, Warp { enabled: true, original_bpm: Some(90.0), mode: WarpMode::Complex, ..Warp::default() });
    let cid = b.p.alloc_clip_id();
    b.p.clips.insert(cid, Clip::new("s", Color(0), sid, 8 * PPQ));
    b.place(s, cid, 0);
    let ram = cache(StreamSettings::OFF);
    let disk = cache(ALL);
    let a = render_with(&b.p, &ram, 1, &[t, s]);
    let d = render_with(&b.p, &disk, 1, &[t, s]);
    assert_eq!(disk.streamed_files(), 1);
    assert!(peak(a.stems[1].2.channel(0)) > 0.05, "the stretched clip plays");
    same_bits(&a.mix, &d.mix, "converted + stretched mix");
}

#[test]
fn streamed_clips_render_the_same_on_any_thread_count() {
    let path = long_wav("reads.wav", 9.0);
    let (mut p, ids) = reads(&path);
    // A group over two of them, so there are levels.
    let g = p.alloc_track_id();
    p.tracks.push(Track::new(g, TrackKind::Group, "group", Color(0)));
    p.track_mut(ids[0]).unwrap().parent = Some(g);
    p.track_mut(ids[1]).unwrap().parent = Some(g);
    let ram = render_with(&p, &cache(StreamSettings::OFF), 1, &[]);
    let disk = cache(ALL);
    for threads in [1, 2, 4] {
        same_bits(&ram.mix, &render_with(&p, &disk, threads, &[]).mix, &format!("{threads} threads"));
    }
}

// ───────────────────────────── Live ─────────────────────────────

fn live(p: &Project, threads: usize, streaming: StreamSettings) -> (EngineHandle, AudioProcessor) {
    let (h, proc_) = Engine::new(EngineConfig { audio_threads: threads, streaming, ..EngineConfig::headless(SR) }, Arc::new(FallbackFactory)).unwrap();
    let mut proc_ = proc_.unwrap();
    h.update_project(p);
    h.flush();
    // The files load on the cache's thread, then the graph recompiles.
    let t0 = std::time::Instant::now();
    while h.samples_loading() {
        assert!(t0.elapsed() < Duration::from_secs(20), "the files never loaded");
        std::thread::sleep(Duration::from_millis(2));
    }
    h.flush();
    let mut buf = AudioBuffer::new(2, BLOCK);
    proc_.process_planar(&mut buf);
    (h, proc_)
}

/// `blocks` blocks; `step(h, k)` runs before block `k`. With `settle` the
/// loader is waited for before every block (a disk always fast enough).
fn run(h: &EngineHandle, proc_: &mut AudioProcessor, blocks: usize, settle: bool, mut step: impl FnMut(&EngineHandle, usize)) -> Vec<f32> {
    let mut out = Vec::with_capacity(blocks * BLOCK * 2);
    let mut buf = AudioBuffer::new(2, BLOCK);
    for k in 0..blocks {
        step(h, k);
        if settle {
            assert!(h.streams().wait_settled(Duration::from_secs(5)), "the loader keeps up");
        }
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

/// Play with a loop, edit (a recompile: the readers carry over warm), stop,
/// seek far into the file while stopped, play again from there.
fn script(p: &Project, ids: &[TrackId]) -> impl FnMut(&EngineHandle, usize) {
    let mut edited = p.clone();
    let ids = ids.to_vec();
    move |h, k| match k {
        0 => {
            h.set_loop(Some((PPQ, 7 * PPQ)));
            h.play();
        }
        300 => {
            edited.track_mut(ids[0]).unwrap().volume_db = -2.0;
            h.update_project(&edited);
            h.flush();
        }
        700 => h.stop(),
        702 => {
            h.set_loop(None);
            h.seek(9 * PPQ + PPQ / 3);
        }
        720 => h.play(),
        _ => {}
    }
}

#[test]
fn live_playback_streams_bit_identical_to_ram_through_loops_edits_and_seeks() {
    let path = long_wav("reads.wav", 9.0);
    let (p, ids) = reads(&path);
    let (h1, mut p1) = live(&p, 1, StreamSettings::OFF);
    let ram = run(&h1, &mut p1, 900, false, script(&p, &ids));
    assert!(peak(&ram) > 0.1);
    assert_eq!(h1.stream_stats().files, 0);
    for threads in [1, 4] {
        let (h, mut proc_) = live(&p, threads, ALL);
        let disk = run(&h, &mut proc_, 900, true, script(&p, &ids));
        let stats = h.stream_stats();
        assert_eq!(stats.files, 1, "the file streamed");
        assert!(stats.buffer_bytes > 0 && stats.buffer_bytes < 32 << 20, "pages, not the file: {} bytes", stats.buffer_bytes);
        assert_eq!(h.audio_health().stream_underruns, 0, "every page was there in time");
        same_vec(&ram, &disk, &format!("streamed on {threads} threads"));
    }
}

/// A jump while playing to a place nothing asked for: the first reads find
/// no page, play silence and count; the next blocks play the file again.
#[test]
fn a_page_the_disk_has_not_delivered_is_silence_and_is_counted() {
    let path = long_wav("reads.wav", 9.0);
    let mut b = P::new(120.0);
    let t = b.track(TrackKind::Audio, "plain");
    let sid = source(&mut b.p, &path, Warp::default());
    let cid = b.p.alloc_clip_id();
    b.p.clips.insert(cid, Clip::new("plain", Color(0), sid, 16 * PPQ));
    b.place(t, cid, 0);
    let (h, mut proc_) = live(&b.p, 1, ALL);
    assert!(h.streams().wait_settled(Duration::from_secs(5)));
    let mut buf = AudioBuffer::new(2, BLOCK);
    h.play();
    for _ in 0..20 {
        proc_.process_planar(&mut buf);
    }
    assert_eq!(h.audio_health().stream_underruns, 0, "the start was pinned and the pages ahead asked for");
    assert!(peak(buf.channel(0)) > 0.1);
    // Into a page no one asked for (16 s at 48 kHz is ~47 pages; this is ~35).
    let target = 16 * PPQ * 35 / 47;
    assert!((target as f64 / PPQ as f64 * 24_000.0) as usize / PAGE > 4);
    h.seek(target);
    proc_.process_planar(&mut buf);
    assert!(h.audio_health().stream_underruns >= 1, "the late page was counted");
    // The very first read is the one that asks for the page, so it is silence.
    // The right channel is read after the left one's whole block: on a fast
    // disk the page can be there by then (it was on the Windows runner).
    assert_eq!(buf.channel(0)[0], 0.0, "and played as silence");
    assert!(h.streams().wait_settled(Duration::from_secs(5)));
    let counted = h.audio_health().stream_underruns;
    for _ in 0..40 {
        assert!(h.streams().wait_settled(Duration::from_secs(5)));
        proc_.process_planar(&mut buf);
    }
    assert!(peak(buf.channel(0)) > 0.1, "playback goes on from disk");
    assert_eq!(h.audio_health().stream_underruns, counted, "no more misses once it caught up");
}

#[test]
fn neither_the_audio_thread_nor_the_workers_allocate_while_clips_stream() {
    let _serial = WORKER_COUNT_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let path = long_wav("reads.wav", 9.0);
    let (p, ids) = reads(&path);
    let (h, mut proc_) = live(&p, 4, ALL);
    h.set_loop(Some((PPQ, 7 * PPQ)));
    h.play();
    let mut buf = AudioBuffer::new(2, BLOCK);
    // Warm-up: installs the graph, wakes every worker once.
    for _ in 0..8 {
        proc_.process_planar(&mut buf);
    }
    assert_eq!(proc_.audio_threads(), 4);
    let pool = proc_.pool_id();
    let blocks = |proc_: &mut AudioProcessor, buf: &mut AudioBuffer, n: usize| {
        count_allocs_in_pools(&[pool], || {
            for _ in 0..n {
                proc_.process_planar(buf);
            }
        })
        .1
    };
    assert_eq!(blocks(&mut proc_, &mut buf, 600), 0, "looping playback from disk");
    // Misses (the pages are asked for from the audio thread), a stop and
    // the cue while stopped, a recompile with the readers carried over.
    h.seek(10 * PPQ);
    assert_eq!(blocks(&mut proc_, &mut buf, 50), 0, "a jump to pages not loaded");
    h.stop();
    h.seek(4 * PPQ);
    let mut edited = p.clone();
    edited.track_mut(ids[1]).unwrap().volume_db = -6.0;
    h.update_project(&edited);
    h.flush();
    assert_eq!(blocks(&mut proc_, &mut buf, 50), 0, "stopped: cueing, graph swap");
    h.play();
    assert_eq!(blocks(&mut proc_, &mut buf, 200), 0, "playing again");
    assert!(h.stream_stats().files >= 1);
}
