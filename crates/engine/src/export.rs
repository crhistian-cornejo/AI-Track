//! Offline rendering and export: mixdown to WAV or MP3, and stems.
//!
//! Rendering runs the exact same graph code as live playback, just driven by
//! a loop instead of a device: the project is compiled with fresh processor
//! instances, played from the range start, paused at the range end (so
//! notes release) and run on for the tail. The first *total-latency*
//! samples are dropped, so the result is PDC-aligned from the range start.
//!
//! The render happens at the session rate; if the export rate differs the
//! result is resampled with the high-quality sinc resampler. WAV is written
//! as 16-bit (optional TPDF dither, optionally noise-shaped), 24-bit or
//! 32-bit float. MP3 is always 320 kbps CBR joint stereo through a
//! dynamically loaded LAME (see [`crate::mp3`]). Stems are always WAV.

use crate::compile::{compile, CompileContext};
use crate::factory::ProcessorFactory;
use crate::mp3::{Lame, Mp3Tags};
use crate::rt::{AudioProcessor, RtCommand, RtQueues, TransportInit};
use crate::sample_cache::{resample, SampleCache};
use crate::shared::ProcessorPool;
use crate::status::SharedStatus;
use crate::EngineError;
use aitrack_dsp::AudioBuffer;
use aitrack_model::{AudioSource, AutomationTarget, Placement, PlacementId, Project, Source, Tick, Track, TrackId, TrackKind, Color, Warp};
use rtrb::RingBuffer;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// What part of the song to render.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenderRange {
    /// From 0 to the end of the last arrangement clip (at least 8 bars).
    Song,
    /// The project's loop region.
    Loop,
    /// An explicit selection in ticks.
    Ticks(Tick, Tick),
}

/// Dither for 16-bit output (ignored at 24-bit and float).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Dither {
    None,
    /// Triangular PDF, ±1 LSB.
    #[default]
    Tpdf,
    /// TPDF with first-order error-feedback noise shaping.
    TpdfShaped,
}

/// WAV sample format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WavFormat {
    Int16(Dither),
    Int24,
    Float32,
}

impl WavFormat {
    pub fn bits(self) -> u16 {
        match self {
            Self::Int16(_) => 16,
            Self::Int24 => 24,
            Self::Float32 => 32,
        }
    }
}

/// Progress callback: receives 0..1, returns `false` to cancel.
pub type Progress<'a> = &'a mut dyn FnMut(f32) -> bool;

/// Rendering options shared by every export.
#[derive(Clone)]
pub struct RenderOptions {
    pub range: RenderRange,
    /// Rate the graph runs at (normally the session/device rate).
    pub session_rate: f64,
    /// Rate of the written file; resampled (sinc) when it differs.
    pub export_rate: u32,
    pub block: usize,
    /// Seconds rendered after the range end with the transport stopped, so
    /// reverbs and releases ring out.
    pub tail_seconds: f64,
    /// Peak-normalise to this many dBFS (e.g. `-0.3`). For stems one common
    /// gain is used, computed from the loudest stem, so they still sum.
    pub normalize_db: Option<f32>,
    /// Decoded audio to reuse (the live engine's cache); `None` decodes.
    pub samples: Option<SampleCache>,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self {
            range: RenderRange::Song,
            session_rate: 48_000.0,
            export_rate: 48_000,
            block: 512,
            tail_seconds: 2.0,
            normalize_db: None,
            samples: None,
        }
    }
}

/// A finished render at `sample_rate`.
pub struct Rendered {
    pub sample_rate: u32,
    pub mix: AudioBuffer,
    /// `(track, name, audio)` for each requested stem, same length as `mix`.
    pub stems: Vec<(TrackId, String, AudioBuffer)>,
}

fn resolve_range(project: &Project, range: RenderRange) -> Result<(Tick, Tick), EngineError> {
    let (a, b) = match range {
        RenderRange::Song => (0, project.arrangement_end()),
        RenderRange::Loop => project.loop_region.ok_or_else(|| EngineError::Export("the project has no loop region".into()))?,
        RenderRange::Ticks(a, b) => (a, b),
    };
    if b <= a {
        return Err(EngineError::Export("empty render range".into()));
    }
    Ok((a, b))
}

fn planar(frames: usize) -> AudioBuffer {
    AudioBuffer::new(2, frames)
}

/// Render the project offline (mix plus optional stem taps).
pub fn render(
    project: &Project,
    factory: &dyn ProcessorFactory,
    opts: &RenderOptions,
    stems: &[TrackId],
    mut progress: Option<Progress<'_>>,
) -> Result<Rendered, EngineError> {
    let (start, end) = resolve_range(project, opts.range)?;
    let sr = opts.session_rate;
    let block = opts.block.clamp(16, 8192);
    let samples = opts.samples.clone().unwrap_or_else(SampleCache::blocking);
    let mut pool = ProcessorPool::new();
    let mut cx = CompileContext {
        factory,
        pool: &mut pool,
        samples: &samples,
        sample_rate: sr,
        max_frames: block,
        prev_meters: None,
        taps: stems.to_vec(),
        analysis: None,
    };
    let graph = compile(project, &mut cx)?;
    let latency = graph.total_latency as usize;
    let tempo = graph.tempo.clone();

    let (mut graph_tx, graph_rx) = RingBuffer::new(2);
    let (retire_tx, _retire_rx) = RingBuffer::new(2);
    let (mut cmd_tx, cmd_rx) = RingBuffer::new(16);
    let q = RtQueues { graph_rx, retire_tx, cmd_rx, midi_rx: Vec::new(), input_rx: None };
    let status = Arc::new(SharedStatus::new());
    let init = TransportInit { playing: true, position_ticks: start as f64, ..TransportInit::default() };
    let mut runner = AudioProcessor::new(q, status, sr, block, init);
    graph_tx.push(Box::new(graph)).map_err(|_| EngineError::Export("graph queue".into()))?;

    let play_frames = (tempo.tick_to_samples(end, sr) - tempo.tick_to_samples(start, sr)).round().max(0.0) as usize;
    let tail = (opts.tail_seconds.max(0.0) * sr).round() as usize;
    let out_frames = play_frames + tail;
    let total = latency + out_frames;

    let mut mix = planar(out_frames);
    let mut stem_bufs: Vec<(TrackId, String, AudioBuffer)> = stems
        .iter()
        .filter_map(|id| project.track(*id).map(|t| (*id, t.name.clone(), planar(out_frames))))
        .collect();
    let mut buf = planar(block);
    let mut done = 0usize;
    let mut paused = false;
    while done < total {
        let mut n = block.min(total - done);
        if !paused {
            if done == play_frames {
                let _ = cmd_tx.push(RtCommand::Pause);
                paused = true;
            } else if done < play_frames && done + n > play_frames {
                n = play_frames - done;
            }
        }
        buf.set_frames(n);
        runner.process_planar(&mut buf);
        // Global output index of this block's first frame (after dropping latency).
        let copy = |src: &[f32], dst: &mut [f32]| {
            for (k, s) in src.iter().enumerate() {
                let g = done + k;
                if g >= latency {
                    dst[g - latency] = *s;
                }
            }
        };
        for ch in 0..2 {
            copy(buf.channel(ch), mix.channel_mut(ch));
        }
        if let Some(g) = runner.graph_mut() {
            for (id, _, out) in &mut stem_bufs {
                if let Some(t) = g.track_index(*id).and_then(|i| g.tracks[i].tap.as_deref_mut()) {
                    for ch in 0..2 {
                        copy(&t.block.channel(ch)[..n], out.channel_mut(ch));
                    }
                    t.block.clear();
                }
            }
        }
        done += n;
        if let Some(p) = progress.as_mut()
            && !p(done as f32 / total.max(1) as f32) {
                return Err(EngineError::Cancelled);
            }
    }
    drop(runner);

    // Normalise (one gain for the mix, one shared gain for all stems).
    if let Some(db) = opts.normalize_db {
        let target = aitrack_dsp::db_to_gain(db);
        let peak_of = |b: &AudioBuffer| (0..b.channels()).map(|c| b.peak(c)).fold(0.0f32, f32::max);
        let p = peak_of(&mix);
        if p > 1e-9 {
            mix.apply_gain(target / p);
        }
        let sp = stem_bufs.iter().map(|s| peak_of(&s.2)).fold(0.0f32, f32::max);
        if sp > 1e-9 {
            for s in &mut stem_bufs {
                s.2.apply_gain(target / sp);
            }
        }
    }

    // Resample to the export rate.
    let rate_in = sr.round() as u32;
    let rate_out = opts.export_rate.max(1);
    if rate_out != rate_in {
        mix = resample_buffer(&mix, rate_in, rate_out)?;
        for s in &mut stem_bufs {
            s.2 = resample_buffer(&s.2, rate_in, rate_out)?;
        }
    }
    Ok(Rendered { sample_rate: rate_out, mix, stems: stem_bufs })
}

/// Resample a planar buffer (sinc, delay-compensated).
pub fn resample_buffer(buf: &AudioBuffer, from: u32, to: u32) -> Result<AudioBuffer, EngineError> {
    let chans: Vec<Vec<f32>> = (0..buf.channels()).map(|c| buf.channel(c).to_vec()).collect();
    let out = resample(&chans, from, to).map_err(EngineError::Export)?;
    let frames = out.first().map_or(0, Vec::len);
    let mut b = AudioBuffer::new(buf.channels(), frames);
    for (c, data) in out.iter().enumerate() {
        b.channel_mut(c).copy_from_slice(&data[..frames]);
    }
    Ok(b)
}

/// Render `range` at `sample_rate` without tail or normalisation (graph
/// tests, freeze, bounce).
pub fn render_offline(
    project: &Project,
    factory: &dyn ProcessorFactory,
    range_ticks: (Tick, Tick),
    sample_rate: f64,
    block: usize,
) -> Result<AudioBuffer, EngineError> {
    let opts = RenderOptions {
        range: RenderRange::Ticks(range_ticks.0, range_ticks.1),
        session_rate: sample_rate,
        export_rate: sample_rate.round() as u32,
        block,
        tail_seconds: 0.0,
        normalize_db: None,
        samples: None,
    };
    Ok(render(project, factory, &opts, &[], None)?.mix)
}

/// Like [`render_offline`] but with a sample cache (e.g. pre-filled audio).
pub fn render_offline_with(
    project: &Project,
    factory: &dyn ProcessorFactory,
    range_ticks: (Tick, Tick),
    sample_rate: f64,
    block: usize,
    samples: SampleCache,
) -> Result<AudioBuffer, EngineError> {
    let opts = RenderOptions {
        range: RenderRange::Ticks(range_ticks.0, range_ticks.1),
        session_rate: sample_rate,
        export_rate: sample_rate.round() as u32,
        block,
        tail_seconds: 0.0,
        normalize_db: None,
        samples: Some(samples),
    };
    Ok(render(project, factory, &opts, &[], None)?.mix)
}

// ───────────────────────────── Consolidate ─────────────────────────────

/// Render what the audio clips `placements` of `track` play over `range`
/// (gain, fades, volume envelope, loops and warp exactly as playback does
/// them, before the track's devices and mixer), as one stereo buffer at
/// `sample_rate`. Muted clips are silent and gaps stay silent. Runs the
/// offline renderer on a scratch project that holds only those clips, so
/// call it off the audio and UI threads.
pub fn render_clips(
    project: &Project,
    track: TrackId,
    placements: &[PlacementId],
    range: (Tick, Tick),
    sample_rate: f64,
    samples: Option<SampleCache>,
) -> Result<AudioBuffer, EngineError> {
    let scratch = clips_only(project, track, placements)?;
    let samples = samples.unwrap_or_else(SampleCache::blocking);
    render_offline_with(&scratch, &crate::factory::NullFactory, range, sample_rate, 512, samples)
}

/// A project with one plain audio track (no devices, unity gain, centred)
/// holding copies of the given placements, the tempo of `project` and
/// nothing else.
fn clips_only(project: &Project, track: TrackId, placements: &[PlacementId]) -> Result<Project, EngineError> {
    let from = project.track(track).ok_or_else(|| EngineError::Export("the track is gone".into()))?;
    let mut p = Project::new(project.name.clone());
    p.tempo = project.tempo.clone();
    p.loop_region = None;
    p.sample_rate_hint = project.sample_rate_hint;
    // The tempo lane on Main folds into the tempo map.
    if let (Some(main), Some(scratch_main)) = (project.tracks.iter().find(|t| t.kind == TrackKind::Main), p.tracks.first_mut()) {
        scratch_main.automation = main.automation.iter().filter(|l| l.target == AutomationTarget::Tempo).cloned().collect();
    }
    let id = p.alloc_track_id();
    let mut lane = Track::new(id, TrackKind::Audio, "Consolidate", Color(0));
    let mut sources = std::collections::BTreeMap::new();
    for want in placements {
        let pl = from.arrangement.iter().find(|pl| pl.id == *want).ok_or_else(|| EngineError::Export("a clip is gone".into()))?;
        let clip = project.clips.get(&pl.clip).ok_or_else(|| EngineError::Export("a clip is gone".into()))?;
        let source = match project.sources.get(&clip.source) {
            Some(s @ Source::Audio(_)) => s.clone(),
            _ => return Err(EngineError::Export("only audio clips are rendered".into())),
        };
        let sid = *sources.entry(clip.source).or_insert_with(|| {
            let sid = p.alloc_source_id();
            p.sources.insert(sid, source);
            sid
        });
        let cid = p.alloc_clip_id();
        p.clips.insert(cid, aitrack_model::Clip { source: sid, ..clip.clone() });
        let pid = p.alloc_placement_id();
        lane.arrangement.push(Placement { id: pid, clip: cid, start: pl.start, lane: 0 });
    }
    lane.arrangement.sort_by_key(|pl| pl.start);
    p.tracks.insert(0, lane);
    Ok(p)
}

/// [`render_clips`] written to `path` as a 32-bit float WAV (mono when every
/// part is mono). Returns the file as a source to put in the project: warped
/// at the song tempo when the tempo does not change, so it keeps following
/// the song like the clips it replaces.
pub fn consolidate_to_wav(
    project: &Project,
    track: TrackId,
    placements: &[PlacementId],
    range: (Tick, Tick),
    sample_rate: u32,
    path: &Path,
    samples: Option<SampleCache>,
) -> Result<AudioSource, EngineError> {
    let mut buf = render_clips(project, track, placements, range, sample_rate as f64, samples)?;
    let files: Vec<&AudioSource> = project
        .track(track)
        .into_iter()
        .flat_map(|t| t.arrangement.iter().filter(|pl| placements.contains(&pl.id)))
        .filter_map(|pl| match project.sources.get(&project.clips.get(&pl.clip)?.source) {
            Some(Source::Audio(a)) => Some(a),
            _ => None,
        })
        .collect();
    let mono = !files.is_empty() && files.iter().all(|a| a.channels == 1);
    if mono {
        let mut one = AudioBuffer::new(1, buf.frames());
        one.channel_mut(0).copy_from_slice(buf.channel(0));
        buf = one;
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(EngineError::Io)?;
    }
    write_wav(path, &buf, sample_rate, WavFormat::Float32)?;
    let steady = project.tempo.points().len() == 1
        && !project.tracks.iter().any(|t| t.kind == TrackKind::Main && t.automation.iter().any(|l| l.target == AutomationTarget::Tempo && l.enabled && !l.points.is_empty()));
    let warp = if steady { Warp { enabled: true, original_bpm: Some(project.tempo.initial_bpm()), ..Warp::default() } } else { Warp::default() };
    let key = files.first().and_then(|a| a.key).filter(|k| files.iter().all(|a| a.key == Some(*k)));
    Ok(AudioSource { path: path.to_path_buf(), hash: None, sample_rate, channels: buf.channels() as u16, frames: buf.frames() as u64, warp, key })
}

// ───────────────────────────── WAV ─────────────────────────────

/// Small fast RNG for dither (xorshift64*), uniform in [-0.5, 0.5).
struct DitherRng(u64);

impl DitherRng {
    fn uniform(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        ((x.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40) as f32 / (1u64 << 24) as f32) - 0.5
    }
}

/// Quantise to 16-bit with the chosen dither. `state` holds the shaping
/// error per channel.
fn quantize16(x: f32, dither: Dither, rng: &mut DitherRng, err: &mut f32) -> i16 {
    let scale = 32_767.0f32;
    let v = x * scale;
    let q = match dither {
        Dither::None => v.round(),
        Dither::Tpdf => (v + rng.uniform() + rng.uniform()).round(),
        Dither::TpdfShaped => {
            let shaped = v - *err;
            let q = (shaped + rng.uniform() + rng.uniform()).round();
            *err = q - shaped;
            q
        }
    };
    q.clamp(-32_768.0, 32_767.0) as i16
}

/// Write a planar buffer as WAV.
pub fn write_wav(path: &Path, buffer: &AudioBuffer, sample_rate: u32, format: WavFormat) -> Result<(), EngineError> {
    let channels = buffer.channels().max(1) as u16;
    let spec = hound::WavSpec {
        channels,
        sample_rate,
        bits_per_sample: format.bits(),
        sample_format: if format == WavFormat::Float32 { hound::SampleFormat::Float } else { hound::SampleFormat::Int },
    };
    let wav_err = |e: hound::Error| EngineError::Export(e.to_string());
    let mut w = hound::WavWriter::create(path, spec).map_err(wav_err)?;
    let frames = buffer.frames();
    let mut rng = DitherRng(0x9E37_79B9_7F4A_7C15);
    let mut err = vec![0.0f32; channels as usize];
    for f in 0..frames {
        for (c, e) in err.iter_mut().enumerate() {
            let x = buffer.channel(c)[f];
            match format {
                WavFormat::Int16(d) => w.write_sample(quantize16(x, d, &mut rng, e)).map_err(wav_err)?,
                WavFormat::Int24 => {
                    let v = (x * 8_388_607.0).round().clamp(-8_388_608.0, 8_388_607.0) as i32;
                    w.write_sample(v).map_err(wav_err)?
                }
                WavFormat::Float32 => w.write_sample(x).map_err(wav_err)?,
            }
        }
    }
    w.finalize().map_err(wav_err)
}

/// Render and write a WAV mixdown.
pub fn export_wav(
    project: &Project,
    factory: &dyn ProcessorFactory,
    path: &Path,
    opts: &RenderOptions,
    format: WavFormat,
    progress: Option<Progress<'_>>,
) -> Result<(), EngineError> {
    let r = render(project, factory, opts, &[], progress)?;
    write_wav(path, &r.mix, r.sample_rate, format)
}

/// Render and write an MP3 (320 kbps CBR, joint stereo, ID3v2 tags).
/// `opts.export_rate` must be 32 000, 44 100 or 48 000.
pub fn export_mp3(
    project: &Project,
    factory: &dyn ProcessorFactory,
    path: &Path,
    opts: &RenderOptions,
    tags: &Mp3Tags,
    lame: &Lame,
    progress: Option<Progress<'_>>,
) -> Result<(), EngineError> {
    if ![32_000, 44_100, 48_000].contains(&opts.export_rate) {
        return Err(EngineError::Export(format!("MP3 needs 32, 44.1 or 48 kHz, not {} Hz", opts.export_rate)));
    }
    let r = render(project, factory, opts, &[], progress)?;
    let bytes = lame.encode_320_cbr(&r.mix, r.sample_rate, tags)?;
    std::fs::write(path, bytes).map_err(EngineError::Io)
}

/// Keep file names portable.
fn sanitize(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.') { c } else { '_' })
        .collect();
    let s = s.trim().trim_matches('.').to_string();
    if s.is_empty() { "Track".into() } else { s }
}

/// Render the selected tracks (any audio track, group or return) as
/// separate WAV files into `dir`, all starting at the range start, with the
/// same length (tail included), bit depth and rate. Returns the paths.
pub fn export_stems(
    project: &Project,
    factory: &dyn ProcessorFactory,
    dir: &Path,
    opts: &RenderOptions,
    tracks: &[TrackId],
    format: WavFormat,
    progress: Option<Progress<'_>>,
) -> Result<Vec<PathBuf>, EngineError> {
    std::fs::create_dir_all(dir).map_err(EngineError::Io)?;
    let r = render(project, factory, opts, tracks, progress)?;
    let mut paths = Vec::with_capacity(r.stems.len());
    for (i, (_, name, buf)) in r.stems.iter().enumerate() {
        let path = dir.join(format!("{:02} {}.wav", i + 1, sanitize(name)));
        write_wav(&path, buf, r.sample_rate, format)?;
        paths.push(path);
    }
    Ok(paths)
}
