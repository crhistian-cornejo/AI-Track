//! Decoded audio for audio clips, loaded off the audio thread.
//!
//! Files are decoded (hound for WAV, symphonia for everything else),
//! resampled with rubato to the device rate and kept fully in memory behind
//! an `Arc`, shared by every clip that uses them. Long files are streamed
//! from disk instead ([`crate::stream`], decided by [`StreamSettings`]): their
//! [`DecodedAudio`] holds no samples, only the [`StreamFile`] each clip's
//! reader pages through. Both are read with the same interpolation kernel
//! ([`interpolate`] over a [`FrameSource`]), so a streamed file sounds
//! bit-identical to the same file in RAM.

use aitrack_dsp::warp::{detect_transients, warp_audio, warp_audio_mapped, Stretcher, WarpAlgorithm};
use aitrack_model::WarpMode;
use crossbeam_channel::{unbounded, Sender};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};

use crate::stream::{StreamFile, StreamSettings};

/// Planar `f32` audio at `sample_rate`.
#[derive(Debug)]
pub struct DecodedAudio {
    pub channels: Vec<Vec<f32>>,
    pub sample_rate: u32,
    /// Rate of the file before resampling.
    pub source_rate: u32,
    pub frames: usize,
    /// Set for a streamed file: `channels` then holds one empty `Vec` per
    /// channel and the samples are read through a
    /// [`crate::stream::StreamReader`].
    pub stream: Option<Arc<StreamFile>>,
}

impl DecodedAudio {
    pub fn from_planar(channels: Vec<Vec<f32>>, sample_rate: u32) -> Self {
        let frames = channels.iter().map(Vec::len).min().unwrap_or(0);
        init_interpolator();
        Self { channels, sample_rate, source_rate: sample_rate, frames, stream: None }
    }

    /// A file streamed from disk (no samples in memory).
    pub fn streamed(file: Arc<StreamFile>, source_rate: u32) -> Self {
        init_interpolator();
        Self { channels: vec![Vec::new(); file.channels().max(1)], sample_rate: file.sample_rate(), source_rate, frames: file.frames(), stream: Some(file) }
    }

    /// Whether the samples live on disk ([`stream`](Self::stream)).
    pub fn is_streamed(&self) -> bool {
        self.stream.is_some()
    }

    /// Heap bytes occupied by the decoded sample buffers.
    pub fn byte_size(&self) -> usize {
        self.channels.iter().map(|c| c.capacity() * std::mem::size_of::<f32>()).sum()
    }

    /// The exact stored sample at an integer frame; 0 outside the data.
    /// Used for clips that play at their native speed, so playback is
    /// bit-identical to the file.
    #[inline]
    pub fn frame(&self, ch: usize, index: i64) -> f32 {
        if index < 0 || index as usize >= self.frames || self.channels.is_empty() {
            return 0.0;
        }
        // (A streamed file's channels are empty: its clips read the stream.)
        self.channels[ch.min(self.channels.len() - 1)].get(index as usize).copied().unwrap_or(0.0)
    }

    /// Windowed-sinc interpolation at a fractional frame; 0 outside the data.
    ///
    /// Linear interpolation dulls the top octave (−3 dB at a quarter of the
    /// sample rate at half-sample offsets), so anything read at a fractional
    /// position goes through a 16-tap Kaiser-windowed sinc instead. The table
    /// is built once off the audio thread (see [`init_interpolator`]).
    #[inline]
    pub fn sample(&self, ch: usize, pos: f64) -> f32 {
        interpolate(self, ch, pos)
    }
}

/// Planar audio the interpolator reads: in memory ([`DecodedAudio`]) or
/// paged from disk ([`crate::stream::StreamReader`]).
pub(crate) trait FrameSource {
    fn channel_count(&self) -> usize;
    fn frame_count(&self) -> usize;
    /// Frames `first .. first + len` of channel `ch` (all inside the data),
    /// when they are stored contiguously.
    fn window(&self, ch: usize, first: usize, len: usize) -> Option<&[f32]>;
    /// Frame `index` (inside the data) of channel `ch`.
    fn at(&self, ch: usize, index: usize) -> f32;
}

impl FrameSource for DecodedAudio {
    #[inline]
    fn channel_count(&self) -> usize {
        self.channels.len()
    }

    #[inline]
    fn frame_count(&self) -> usize {
        self.frames
    }

    #[inline]
    fn window(&self, ch: usize, first: usize, len: usize) -> Option<&[f32]> {
        self.channels[ch].get(first..first + len)
    }

    #[inline]
    fn at(&self, ch: usize, index: usize) -> f32 {
        self.channels[ch].get(index).copied().unwrap_or(0.0)
    }
}

/// [`DecodedAudio::sample`] for any [`FrameSource`]: a 16-tap Kaiser-windowed
/// sinc at a fractional frame, 0 outside the data. The taps are summed in
/// the same order whichever way they are fetched, so the result does not
/// depend on where the samples are stored.
#[inline]
pub(crate) fn interpolate<S: FrameSource + ?Sized>(src: &S, ch: usize, pos: f64) -> f32 {
    let frames = src.frame_count();
    let channels = src.channel_count();
    if channels == 0 || pos <= -(SINC_HALF as f64) || pos >= (frames + SINC_HALF) as f64 {
        return 0.0;
    }
    let ch = ch.min(channels - 1);
    let floor = pos.floor();
    let base = floor as i64;
    let phase = (pos - floor) * SINC_PHASES as f64;
    let p0 = (phase as usize).min(SINC_PHASES - 1);
    let blend = (phase - p0 as f64) as f32;
    let table = sinc_table();
    let row0 = &table[p0 * SINC_TAPS..(p0 + 1) * SINC_TAPS];
    let row1 = &table[(p0 + 1) * SINC_TAPS..(p0 + 2) * SINC_TAPS];
    let first = base - SINC_HALF as i64 + 1;
    let mut acc = 0.0f32;
    if first >= 0 && (first as usize + SINC_TAPS) <= frames {
        if let Some(window) = src.window(ch, first as usize, SINC_TAPS) {
            for j in 0..SINC_TAPS {
                acc += window[j] * (row0[j] + (row1[j] - row0[j]) * blend);
            }
        } else {
            for j in 0..SINC_TAPS {
                acc += src.at(ch, first as usize + j) * (row0[j] + (row1[j] - row0[j]) * blend);
            }
        }
    } else {
        for j in 0..SINC_TAPS {
            let idx = first + j as i64;
            if idx >= 0 && (idx as usize) < frames {
                acc += src.at(ch, idx as usize) * (row0[j] + (row1[j] - row0[j]) * blend);
            }
        }
    }
    acc
}

const SINC_TAPS: usize = 16;
const SINC_HALF: usize = SINC_TAPS / 2;
const SINC_PHASES: usize = 512;

/// Build the interpolation table. Call off the audio thread before any
/// playback (every [`DecodedAudio`] constructor does), so the audio thread
/// only ever reads it.
pub fn init_interpolator() {
    let _ = sinc_table();
}

/// `(SINC_PHASES + 1)` rows of `SINC_TAPS` coefficients; row `p` interpolates
/// at fraction `p / SINC_PHASES`. Each row sums to 1 (unity gain at DC).
fn sinc_table() -> &'static [f32] {
    static TABLE: std::sync::OnceLock<Box<[f32]>> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        // Slightly below Nyquist so the transition band does not fold back.
        const CUTOFF: f64 = 0.94;
        const BETA: f64 = 9.0;
        let i0 = |x: f64| -> f64 {
            let (mut sum, mut term, mut k) = (1.0, 1.0, 1.0);
            while term > 1e-12 * sum {
                term *= (x / (2.0 * k)) * (x / (2.0 * k));
                sum += term;
                k += 1.0;
            }
            sum
        };
        let norm = i0(BETA);
        let mut table = vec![0.0f32; (SINC_PHASES + 1) * SINC_TAPS];
        for p in 0..=SINC_PHASES {
            let frac = p as f64 / SINC_PHASES as f64;
            let mut row = [0.0f64; SINC_TAPS];
            let mut sum = 0.0;
            for (j, c) in row.iter_mut().enumerate() {
                let x = j as f64 - (SINC_HALF as f64 - 1.0) - frac;
                let sinc = if x.abs() < 1e-12 { 1.0 } else { (std::f64::consts::PI * CUTOFF * x).sin() / (std::f64::consts::PI * CUTOFF * x) };
                let w = x / SINC_HALF as f64;
                let window = if w.abs() >= 1.0 { 0.0 } else { i0(BETA * (1.0 - w * w).sqrt()) / norm };
                *c = CUTOFF * sinc * window;
                sum += *c;
            }
            for (j, c) in row.iter().enumerate() {
                table[p * SINC_TAPS + j] = (c / sum) as f32;
            }
        }
        table.into_boxed_slice()
    })
}

/// Decode an audio file to planar `f32` at its own rate.
pub fn decode_file(path: &Path) -> Result<(Vec<Vec<f32>>, u32), String> {
    let is_wav = path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("wav"));
    if is_wav
        && let Ok(r) = decode_wav(path) {
            return Ok(r);
        }
    decode_symphonia(path)
}

fn decode_wav(path: &Path) -> Result<(Vec<Vec<f32>>, u32), String> {
    let mut reader = hound::WavReader::open(path).map_err(|e| e.to_string())?;
    let spec = reader.spec();
    let ch = spec.channels.max(1) as usize;
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().collect::<Result<_, _>>().map_err(|e| e.to_string())?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1i64 << (spec.bits_per_sample.max(1) - 1)) as f32;
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| v as f32 * scale))
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())?
        }
    };
    Ok((deinterleave(&interleaved, ch), spec.sample_rate))
}

fn deinterleave(data: &[f32], ch: usize) -> Vec<Vec<f32>> {
    let frames = data.len() / ch;
    let mut out = vec![Vec::with_capacity(frames); ch];
    for frame in data.chunks_exact(ch) {
        for (c, s) in frame.iter().enumerate() {
            out[c].push(*s);
        }
    }
    out
}

fn decode_symphonia(path: &Path) -> Result<(Vec<Vec<f32>>, u32), String> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
    use symphonia::core::errors::Error as SError;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    // Gapless: an MP3 or M4A drops its encoder delay and padding, so the first
    // hit lands on the grid instead of ~25-50 ms late.
    let format_opts = FormatOptions { enable_gapless: true, ..Default::default() };
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &format_opts, &MetadataOptions::default())
        .map_err(|e| e.to_string())?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or("no audio track")?;
    let track_id = track.id;
    let mut rate = track.codec_params.sample_rate.unwrap_or(0);
    let mut decoder =
        symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default()).map_err(|e| e.to_string())?;
    let mut out: Vec<Vec<f32>> = Vec::new();
    let mut sample_buf: Option<SampleBuffer<f32>> = None;
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SError::ResetRequired) => break,
            Err(e) => return Err(e.to_string()),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(SError::DecodeError(_)) => continue,
            Err(e) => return Err(e.to_string()),
        };
        let spec = *decoded.spec();
        rate = spec.rate;
        let ch = spec.channels.count().max(1);
        let needed = decoded.capacity() as u64;
        if sample_buf.as_ref().is_none_or(|b| b.capacity() < decoded.capacity() * ch) {
            sample_buf = Some(SampleBuffer::new(needed, spec));
        }
        let buf = sample_buf.as_mut().expect("just created");
        buf.copy_interleaved_ref(decoded);
        if out.len() != ch {
            out = vec![Vec::new(); ch];
        }
        for frame in buf.samples().chunks_exact(ch) {
            for (c, s) in frame.iter().enumerate() {
                out[c].push(*s);
            }
        }
    }
    if rate == 0 {
        return Err("unknown sample rate".into());
    }
    Ok((out, rate))
}

/// Resample planar audio with rubato's windowed-sinc resampler at high
/// quality (256-tap sinc, 256× oversampled table, cubic interpolation,
/// Blackman-Harris² window). Output length is `ceil(frames · to / from)`
/// and the result is time-aligned: rubato's sinc resampler starts reading
/// half a kernel early, so its `output_delay()` is already compensated and
/// must not be trimmed again (that cut ~3 ms off every attack).
pub fn resample(channels: &[Vec<f32>], from: u32, to: u32) -> Result<Vec<Vec<f32>>, String> {
    use rubato::{calculate_cutoff, Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction};
    if from == to || channels.is_empty() {
        return Ok(channels.to_vec());
    }
    if from == 0 || to == 0 {
        return Err("invalid sample rate".into());
    }
    let frames = channels.iter().map(Vec::len).min().unwrap_or(0);
    let chunk = 1024;
    let sinc_len = 256;
    let window = WindowFunction::BlackmanHarris2;
    let params = SincInterpolationParameters {
        sinc_len,
        f_cutoff: calculate_cutoff(sinc_len, window),
        interpolation: SincInterpolationType::Cubic,
        oversampling_factor: 256,
        window,
    };
    let mut rs = SincFixedIn::<f32>::new(to as f64 / from as f64, 1.0, params, chunk, channels.len()).map_err(|e| e.to_string())?;
    let expected = (frames as u64 * to as u64).div_ceil(from as u64) as usize;
    let mut out = vec![Vec::with_capacity(expected + chunk); channels.len()];
    let mut pos = 0;
    while pos < frames {
        let need = rs.input_frames_next();
        let end = (pos + need).min(frames);
        let slices: Vec<&[f32]> = channels.iter().map(|c| &c[pos..end]).collect();
        let res = if end - pos == need { rs.process(&slices, None) } else { rs.process_partial(Some(&slices), None) }
            .map_err(|e| e.to_string())?;
        for (o, r) in out.iter_mut().zip(res) {
            o.extend_from_slice(&r);
        }
        pos = end;
    }
    while out[0].len() < expected {
        let res = rs.process_partial::<&[f32]>(None, None).map_err(|e| e.to_string())?;
        if res[0].is_empty() {
            break;
        }
        for (o, r) in out.iter_mut().zip(res) {
            o.extend_from_slice(&r);
        }
    }
    for o in &mut out {
        o.resize(expected, 0.0);
    }
    Ok(out)
}

/// Decode `path` and resample it to `rate`.
pub fn load_audio(path: &Path, rate: u32) -> Result<DecodedAudio, String> {
    let (chans, src_rate) = decode_file(path)?;
    let chans = resample(&chans, src_rate, rate)?;
    let mut d = DecodedAudio::from_planar(chans, rate);
    d.source_rate = src_rate;
    Ok(d)
}

fn is_wav(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("wav"))
}

/// `path` at `rate`, streamed from disk when `streaming` says so (see
/// [`crate::stream`]), else decoded into memory ([`load_audio`]). A WAV that
/// can be read in place at the device rate is not decoded at all; anything
/// else long enough is decoded, resampled and written to a temporary float
/// WAV, which then streams.
pub fn load_source(path: &Path, rate: u32, streaming: StreamSettings) -> Result<DecodedAudio, String> {
    if streaming.enabled
        && is_wav(path)
        && let Some(f) = StreamFile::open_wav(path)
        && f.sample_rate() == rate
        && streaming.streams(f.frames(), f.channels(), rate)
        // The RAM path reads it with hound: only stream what hound reads the same way.
        && hound::WavReader::open(path).is_ok_and(|r| r.duration() as usize == f.frames() && r.spec().channels as usize == f.channels())
    {
        return Ok(DecodedAudio::streamed(Arc::new(f), rate));
    }
    let d = load_audio(path, rate)?;
    if streaming.streams(d.frames, d.channels.len(), rate) {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("audio");
        match StreamFile::from_decoded(&d.channels, rate, name) {
            Ok(f) => return Ok(DecodedAudio::streamed(Arc::new(f), d.source_rate)),
            Err(e) => log::warn!("could not stream {} from disk ({e}); it stays in memory", path.display()),
        }
    }
    Ok(d)
}

/// Default decoded sample cache budget (512 MB) before LRU eviction.
pub const DEFAULT_CACHE_BUDGET_BYTES: usize = 512 * 1024 * 1024;

enum Entry {
    Loading,
    Ready { audio: Arc<DecodedAudio>, used: u64 },
    Failed(String),
}

type Key = (PathBuf, u32);

/// A stretched render and the transients of the stretched audio.
pub(crate) type Warped = (Arc<DecodedAudio>, Arc<[usize]>);

enum WarpState {
    Computing,
    Ready { audio: Arc<DecodedAudio>, transients: Arc<[usize]>, used: u64 },
    Failed,
}

/// A time map for a render: `(frame of the source, frame of the render)`
/// anchors, rising. The audio between two anchors is stretched to fit.
pub(crate) type WarpMap = Arc<[(usize, usize)]>;

/// How a render's time follows its source's: one factor for the whole file,
/// or anchors (a file with warp markers).
#[derive(Clone, PartialEq)]
enum Shape {
    /// The bits of the exact factor: a render made for a nearby one would drift.
    Scale(u64),
    Map(WarpMap),
}

/// One stretched render. The source is told apart by its allocation, not by
/// its content: the weak handle keeps that allocation (not the samples)
/// alive, so no other file can ever take its address.
struct WarpEntry {
    src: Weak<DecodedAudio>,
    mode: WarpMode,
    pitch_cents: i32,
    shape: Shape,
    state: WarpState,
}

impl WarpEntry {
    fn same_source(&self, src: *const DecodedAudio, mode: WarpMode, pitch_cents: i32) -> bool {
        std::ptr::eq(self.src.as_ptr(), src) && self.mode == mode && self.pitch_cents == pitch_cents
    }

    fn is_for(&self, src: *const DecodedAudio, mode: WarpMode, pitch_cents: i32, shape: &Shape) -> bool {
        self.same_source(src, mode, pitch_cents) && self.shape == *shape
    }

    fn bytes(&self) -> usize {
        match &self.state {
            WarpState::Ready { audio, transients, .. } => audio.byte_size() + transients.len() * std::mem::size_of::<usize>(),
            _ => 0,
        }
    }
}

#[derive(Default)]
struct CacheState {
    entries: HashMap<Key, Entry>,
    warps: Vec<WarpEntry>,
    /// What went wrong with stretched renders (newest last, a few kept), for
    /// the UI to tell the user. Those clips play in Re-Pitch.
    warp_failures: Vec<String>,
    clock: u64,
}

const MAX_WARP_FAILURES: usize = 16;

impl CacheState {
    fn note_warp_failure(&mut self, what: &str) {
        if self.warp_failures.len() >= MAX_WARP_FAILURES {
            self.warp_failures.remove(0);
        }
        self.warp_failures.push(format!("time-stretching a clip failed ({what}); it plays in Re-Pitch"));
    }
}

fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    payload.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| payload.downcast_ref::<String>().cloned()).unwrap_or_else(|| "panic".into())
}

/// How a stretched render is made (swapped out in tests).
type Warper = Arc<dyn Fn(&DecodedAudio, WarpMode, f64, f64) -> Warped + Send + Sync>;

enum Victim {
    Sample(Key),
    Warp(usize),
}

impl CacheState {
    fn sample_bytes(&self) -> usize {
        self.entries
            .values()
            .filter_map(|e| match e {
                Entry::Ready { audio, .. } => Some(audio.byte_size()),
                _ => None,
            })
            .sum()
    }

    fn warp_bytes(&self) -> usize {
        self.warps.iter().map(WarpEntry::bytes).sum()
    }

    fn total_bytes(&self) -> usize {
        self.sample_bytes() + self.warp_bytes()
    }

    /// Evict, least recently used first and those no compiled graph holds
    /// before those it does, until the files and their stretched renders fit.
    fn trim(&mut self, budget: usize) {
        // Renders of a file that is gone are of no use to anyone.
        self.warps.retain(|w| w.src.strong_count() > 0);
        let mut total = self.total_bytes();
        while total > budget {
            let samples = self.entries.iter().filter_map(|(k, e)| match e {
                Entry::Ready { audio, used } => Some((Arc::strong_count(audio) > 1, *used, audio.byte_size(), Victim::Sample(k.clone()))),
                _ => None,
            });
            let warps = self.warps.iter().enumerate().filter_map(|(i, w)| match &w.state {
                WarpState::Ready { audio, used, .. } => Some((Arc::strong_count(audio) > 1, *used, w.bytes(), Victim::Warp(i))),
                _ => None,
            });
            let Some((_, _, size, victim)) = samples.chain(warps).min_by_key(|(in_use, used, _, _)| (*in_use, *used)) else { break };
            match victim {
                Victim::Sample(key) => {
                    self.entries.remove(&key);
                }
                Victim::Warp(i) => {
                    self.warps.swap_remove(i);
                }
            }
            total = total.saturating_sub(size);
        }
    }
}

fn warp_algo(mode: WarpMode) -> WarpAlgorithm {
    match mode {
        WarpMode::Beats => WarpAlgorithm::Beats,
        WarpMode::Tones => WarpAlgorithm::Tones,
        WarpMode::Texture => WarpAlgorithm::Texture,
        WarpMode::Complex => WarpAlgorithm::Complex,
        WarpMode::ComplexPro => WarpAlgorithm::ComplexPro,
        WarpMode::Repitch => WarpAlgorithm::Repitch,
    }
}

/// `src` with its samples in memory: a streamed source is read whole for the
/// render (the render is kept in RAM, as for any other file); the copy goes
/// when the render is done.
fn in_memory<R>(src: &DecodedAudio, render: impl FnOnce(&DecodedAudio) -> R) -> R {
    match &src.stream {
        Some(f) => {
            let channels = f.read_all().unwrap_or_else(|e| {
                log::warn!("time-stretching {}: {e}", f.path().display());
                vec![vec![0.0; f.frames()]; f.channels()]
            });
            render(&DecodedAudio { source_rate: src.source_rate, ..DecodedAudio::from_planar(channels, src.sample_rate) })
        }
        None => render(src),
    }
}

/// A finished render as the cache keeps it, with the transients Beats mode plays by.
fn finish_warp(channels: Vec<Vec<f32>>, src: &DecodedAudio, mode: WarpMode) -> Warped {
    let mut warped_audio = DecodedAudio::from_planar(channels, src.sample_rate);
    warped_audio.source_rate = src.source_rate;
    let warped_arc = Arc::new(warped_audio);
    let transients: Arc<[usize]> = Arc::from(if mode == WarpMode::Beats {
        let w_refs: Vec<&[f32]> = warped_arc.channels.iter().map(Vec::as_slice).collect();
        detect_transients(&w_refs, warped_arc.sample_rate)
    } else {
        Vec::new()
    });
    (warped_arc, transients)
}

/// Stretch and pitch-shift a whole file. Slow (whole seconds for a long
/// file): never on the control or the audio thread when a loader is running.
fn render_warp(src: &DecodedAudio, mode: WarpMode, time_scale: f64, pitch_ratio: f64) -> Warped {
    in_memory(src, |src| {
        let refs: Vec<&[f32]> = src.channels.iter().map(Vec::as_slice).collect();
        let trans_vec = if mode == WarpMode::Beats { detect_transients(&refs, src.sample_rate) } else { Vec::new() };
        let warped_channels = warp_audio(&refs, src.sample_rate, warp_algo(mode), time_scale, pitch_ratio, (!trans_vec.is_empty()).then_some(trans_vec.as_slice()));
        finish_warp(warped_channels, src, mode)
    })
}

/// [`render_warp`] along a time map: the file's warp markers, each stretch by its own factor.
fn render_warp_mapped(src: &DecodedAudio, mode: WarpMode, map: &[(usize, usize)], pitch_ratio: f64) -> Warped {
    in_memory(src, |src| {
        let refs: Vec<&[f32]> = src.channels.iter().map(Vec::as_slice).collect();
        finish_warp(warp_audio_mapped(&refs, src.sample_rate, warp_algo(mode), map, pitch_ratio, Stretcher::default()), src, mode)
    })
}

/// A stretch waiting for the warp worker. It holds its source, so the file
/// cannot go away mid-render.
struct WarpJob {
    src: Arc<DecodedAudio>,
    mode: WarpMode,
    shape: Shape,
    pitch_ratio: f64,
    pitch_cents: i32,
}

/// Render `shape` of `src`: one factor goes through `warper` (swapped out in
/// tests), a time map through the mapped render.
fn render_shape(warper: &Warper, src: &DecodedAudio, mode: WarpMode, shape: &Shape, pitch_ratio: f64) -> Warped {
    match shape {
        Shape::Scale(bits) => warper(src, mode, f64::from_bits(*bits), pitch_ratio),
        Shape::Map(map) => render_warp_mapped(src, mode, map, pitch_ratio),
    }
}

/// Shared cache of decoded files keyed by `(path, rate)` and of their
/// stretched renders, with background loader and warp threads and one LRU
/// memory budget for both. Control-thread / UI side only.
#[derive(Clone)]
pub struct SampleCache {
    state: Arc<Mutex<CacheState>>,
    budget: Arc<std::sync::atomic::AtomicUsize>,
    jobs: Option<Sender<Key>>,
    warp_jobs: Option<Sender<WarpJob>>,
    warper: Warper,
    /// Which files stream from disk instead of being decoded into RAM.
    streaming: Arc<Mutex<StreamSettings>>,
}

impl SampleCache {
    /// A cache whose misses are loaded on a background thread; `on_loaded`
    /// runs (on that thread) after each file or stretched render finishes, so
    /// the engine can recompile.
    pub fn with_loader(on_loaded: impl Fn() + Send + 'static) -> Self {
        Self::with_loader_and_warper(on_loaded, render_warp)
    }

    pub(crate) fn with_loader_and_warper(on_loaded: impl Fn() + Send + 'static, warper: impl Fn(&DecodedAudio, WarpMode, f64, f64) -> Warped + Send + Sync + 'static) -> Self {
        let warper: Warper = Arc::new(warper);
        let render = warper.clone();
        let on_loaded = Arc::new(Mutex::new(on_loaded));
        let notify = on_loaded.clone();
        let state: Arc<Mutex<CacheState>> = Arc::default();
        let budget = Arc::new(std::sync::atomic::AtomicUsize::new(DEFAULT_CACHE_BUDGET_BYTES));
        let streaming: Arc<Mutex<StreamSettings>> = Arc::default();
        let (tx, rx) = unbounded::<Key>();
        let map = state.clone();
        let budget_bg = budget.clone();
        let streaming_bg = streaming.clone();
        let spawned = std::thread::Builder::new().name("aitrack-sample-loader".into()).spawn(move || {
            while let Ok(key) = rx.recv() {
                let settings = *streaming_bg.lock().unwrap_or_else(|p| p.into_inner());
                let entry = match load_source(&key.0, key.1, settings) {
                    Ok(d) => Ok(Arc::new(d)),
                    Err(e) => {
                        log::warn!("could not load {}: {e}", key.0.display());
                        Err(e)
                    }
                };
                {
                    let mut st = map.lock().unwrap_or_else(|p| p.into_inner());
                    st.clock = st.clock.saturating_add(1);
                    let used = st.clock;
                    let val = match entry {
                        Ok(audio) => Entry::Ready { audio, used },
                        Err(e) => Entry::Failed(e),
                    };
                    st.entries.insert(key, val);
                    let limit = budget_bg.load(std::sync::atomic::Ordering::Relaxed);
                    st.trim(limit);
                }
                (on_loaded.lock().unwrap_or_else(|p| p.into_inner()))();
            }
        });
        if let Err(e) = spawned {
            log::error!("sample loader thread failed to start: {e}");
        }
        // Stretching a whole file takes long: its own thread, so loads never wait behind it.
        let (warp_tx, warp_rx) = unbounded::<WarpJob>();
        let map = state.clone();
        let budget_bg = budget.clone();
        let spawned = std::thread::Builder::new().name("aitrack-warp-worker".into()).spawn(move || {
            while let Ok(job) = warp_rx.recv() {
                let src_ptr = Arc::as_ptr(&job.src);
                let wanted = |st: &CacheState| {
                    st.warps.iter().any(|w| w.is_for(src_ptr, job.mode, job.pitch_cents, &job.shape) && matches!(w.state, WarpState::Computing))
                };
                // A newer stretch of the same clip replaced this one while it waited.
                if !wanted(&map.lock().unwrap_or_else(|p| p.into_inner())) {
                    continue;
                }
                let rendered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| render_shape(&render, &job.src, job.mode, &job.shape, job.pitch_ratio)));
                {
                    let mut st = map.lock().unwrap_or_else(|p| p.into_inner());
                    st.clock = st.clock.saturating_add(1);
                    let used = st.clock;
                    let state = match rendered {
                        Ok((audio, transients)) => WarpState::Ready { audio, transients, used },
                        Err(e) => {
                            log::warn!("time-stretching a clip panicked; it plays in Re-Pitch");
                            st.note_warp_failure(&panic_text(&*e));
                            WarpState::Failed
                        }
                    };
                    if let Some(w) = st.warps.iter_mut().find(|w| w.is_for(src_ptr, job.mode, job.pitch_cents, &job.shape) && matches!(w.state, WarpState::Computing)) {
                        w.state = state;
                    }
                    let limit = budget_bg.load(std::sync::atomic::Ordering::Relaxed);
                    st.trim(limit);
                }
                (notify.lock().unwrap_or_else(|p| p.into_inner()))();
            }
        });
        let warp_jobs = match spawned {
            Ok(_) => Some(warp_tx),
            Err(e) => {
                log::error!("warp worker thread failed to start: {e}");
                None
            }
        };
        Self { state, budget, jobs: Some(tx), warp_jobs, warper, streaming }
    }

    /// A cache without a loader: misses are decoded synchronously (offline
    /// render, tests).
    pub fn blocking() -> Self {
        Self {
            state: Arc::default(),
            budget: Arc::new(std::sync::atomic::AtomicUsize::new(DEFAULT_CACHE_BUDGET_BYTES)),
            jobs: None,
            warp_jobs: None,
            warper: Arc::new(render_warp),
            streaming: Arc::default(),
        }
    }

    /// A blocking cache that renders stretches with `warper`.
    #[cfg(test)]
    pub(crate) fn with_warper(warper: impl Fn(&DecodedAudio, WarpMode, f64, f64) -> Warped + Send + Sync + 'static) -> Self {
        Self { warper: Arc::new(warper), ..Self::blocking() }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, CacheState> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn set_budget_bytes(&self, bytes: usize) {
        let clamped = bytes.max(32 * 1024);
        self.budget.store(clamped, std::sync::atomic::Ordering::Relaxed);
        self.lock().trim(clamped);
    }

    pub fn budget_bytes(&self) -> usize {
        self.budget.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Which files stream from disk. Files already loaded the other way are
    /// forgotten, so the next compile loads them as the setting now says
    /// (the graph playing keeps its copy until then).
    pub fn set_streaming(&self, settings: StreamSettings) {
        *self.streaming.lock().unwrap_or_else(|p| p.into_inner()) = settings;
        self.lock().entries.retain(|_, e| match e {
            Entry::Ready { audio, .. } => audio.is_streamed() == settings.streams(audio.frames, audio.channels.len(), audio.sample_rate),
            _ => true,
        });
    }

    pub fn streaming(&self) -> StreamSettings {
        *self.streaming.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Decoded files that stream from disk (they take no cache memory).
    pub fn streamed_files(&self) -> usize {
        self.lock().entries.values().filter(|e| matches!(e, Entry::Ready { audio, .. } if audio.is_streamed())).count()
    }

    /// Bytes of decoded files and stretched renders together: what the
    /// budget limits.
    pub fn used_bytes(&self) -> usize {
        self.lock().total_bytes()
    }

    /// Bytes of the stretched renders alone (part of [`used_bytes`](Self::used_bytes)).
    pub fn warp_bytes(&self) -> usize {
        self.lock().warp_bytes()
    }

    /// `src` stretched by `time_scale` and shifted by `pitch_ratio`, when
    /// ready. With a loader running a miss queues the render on the warp
    /// worker, returns `None` (the clip plays unstretched meanwhile) and the
    /// loader's callback fires when it is done; the blocking cache renders
    /// now. A stretch that changes nothing returns `src` itself.
    pub(crate) fn warped(&self, src: &Arc<DecodedAudio>, mode: WarpMode, time_scale: f64, pitch_ratio: f64, pitch_cents: i32) -> Option<Warped> {
        if (time_scale - 1.0).abs() < 1e-4 && (pitch_ratio - 1.0).abs() < 1e-4 {
            return Some((src.clone(), Arc::from([])));
        }
        self.warped_as(src, mode, Shape::Scale(time_scale.to_bits()), pitch_ratio, pitch_cents)
    }

    /// [`warped`](Self::warped) along a time map (see [`WarpMap`]): the render
    /// of a file with warp markers, each stretch between two of them by its
    /// own factor. Cached and rendered in the background like any other.
    pub(crate) fn warped_mapped(&self, src: &Arc<DecodedAudio>, mode: WarpMode, map: WarpMap, pitch_ratio: f64, pitch_cents: i32) -> Option<Warped> {
        self.warped_as(src, mode, Shape::Map(map), pitch_ratio, pitch_cents)
    }

    fn warped_as(&self, src: &Arc<DecodedAudio>, mode: WarpMode, shape: Shape, pitch_ratio: f64, pitch_cents: i32) -> Option<Warped> {
        let ptr = Arc::as_ptr(src);
        {
            let mut st = self.lock();
            st.clock = st.clock.saturating_add(1);
            let now = st.clock;
            if let Some(w) = st.warps.iter_mut().find(|w| w.is_for(ptr, mode, pitch_cents, &shape)) {
                return match &mut w.state {
                    WarpState::Ready { audio, transients, used } => {
                        *used = now;
                        Some((audio.clone(), transients.clone()))
                    }
                    WarpState::Computing | WarpState::Failed => None,
                };
            }
            if let Some(tx) = &self.warp_jobs {
                // Only the latest stretch of a clip is worth rendering: drop
                // the ones still waiting for the same source and mode.
                st.warps.retain(|w| !(w.same_source(ptr, mode, pitch_cents) && matches!(w.state, WarpState::Computing)));
                let state = match tx.send(WarpJob { src: src.clone(), mode, shape: shape.clone(), pitch_ratio, pitch_cents }) {
                    Ok(()) => WarpState::Computing,
                    Err(_) => WarpState::Failed,
                };
                st.warps.push(WarpEntry { src: Arc::downgrade(src), mode, pitch_cents, shape, state });
                return None;
            }
        }
        let rendered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| render_shape(&self.warper, src, mode, &shape, pitch_ratio)));
        let mut st = self.lock();
        st.clock = st.clock.saturating_add(1);
        let used = st.clock;
        let (state, out) = match rendered {
            Ok((audio, transients)) => (WarpState::Ready { audio: audio.clone(), transients: transients.clone(), used }, Some((audio, transients))),
            Err(e) => {
                st.note_warp_failure(&panic_text(&*e));
                (WarpState::Failed, None)
            }
        };
        st.warps.push(WarpEntry { src: Arc::downgrade(src), mode, pitch_cents, shape, state });
        let limit = self.budget_bytes();
        st.trim(limit);
        out
    }

    /// Stretched renders that failed (newest last): those clips play in
    /// Re-Pitch, and the UI can say so.
    pub fn warp_failures(&self) -> Vec<String> {
        self.lock().warp_failures.clone()
    }

    /// Put already-decoded audio in the cache (tests, recorded takes).
    pub fn insert(&self, path: impl Into<PathBuf>, audio: Arc<DecodedAudio>) {
        let rate = audio.sample_rate;
        let mut st = self.lock();
        st.clock = st.clock.saturating_add(1);
        let used = st.clock;
        st.entries.insert((path.into(), rate), Entry::Ready { audio, used });
        let limit = self.budget_bytes();
        st.trim(limit);
    }

    /// The decoded file if ready. On a miss, the background cache queues a
    /// load and returns `None`; the blocking cache decodes now.
    pub fn get(&self, path: &Path, rate: u32) -> Option<Arc<DecodedAudio>> {
        let key = (path.to_path_buf(), rate);
        {
            let mut st = self.lock();
            st.clock = st.clock.saturating_add(1);
            let used = st.clock;
            match st.entries.get_mut(&key) {
                Some(Entry::Ready { audio, used: slot_used }) => {
                    *slot_used = used;
                    return Some(audio.clone());
                }
                Some(Entry::Loading | Entry::Failed(_)) => return None,
                None => {
                    if let Some(tx) = &self.jobs {
                        st.entries.insert(key.clone(), Entry::Loading);
                        let _ = tx.send(key);
                        return None;
                    }
                }
            }
        }
        let res = match load_source(path, rate, self.streaming()) {
            Ok(d) => Ok(Arc::new(d)),
            Err(e) => {
                log::warn!("could not load {}: {e}", path.display());
                Err(e)
            }
        };
        let mut st = self.lock();
        st.clock = st.clock.saturating_add(1);
        let used = st.clock;
        let out = res.as_ref().ok().cloned();
        let entry = match res {
            Ok(audio) => Entry::Ready { audio, used },
            Err(e) => Entry::Failed(e),
        };
        st.entries.insert(key, entry);
        let limit = self.budget_bytes();
        st.trim(limit);
        out
    }

    /// Error message for a file that failed to load.
    pub fn error(&self, path: &Path, rate: u32) -> Option<String> {
        match self.lock().entries.get(&(path.to_path_buf(), rate)) {
            Some(Entry::Failed(e)) => Some(e.clone()),
            _ => None,
        }
    }

    pub fn is_loading(&self) -> bool {
        let st = self.lock();
        st.entries.values().any(|e| matches!(e, Entry::Loading)) || st.warps.iter().any(|w| matches!(w.state, WarpState::Computing))
    }

    /// Forget everything (e.g. after a device rate change the old entries
    /// are useless).
    pub fn clear(&self) {
        let mut st = self.lock();
        st.entries.retain(|_, e| matches!(e, Entry::Loading));
        st.warps.retain(|w| matches!(w.state, WarpState::Computing));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_keeps_length_and_frequency() {
        let from = 44_100u32;
        let to = 48_000u32;
        let n = 44_100;
        let sine: Vec<f32> = (0..n).map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / from as f32).sin()).collect();
        let out = resample(&[sine], from, to).unwrap();
        assert_eq!(out[0].len(), 48_000);
        // Zero crossings ≈ 2 × 440 per second.
        let crossings = out[0][1000..47_000].windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
        let expected = 440.0 * 46_000.0 / 48_000.0;
        assert!((crossings as f64 - expected).abs() < 3.0, "{crossings}");
    }

    /// An impulse must land where it sat in the source, at every ratio: a
    /// resampler that trims its filter delay twice eats the attack.
    #[test]
    fn resample_keeps_an_impulse_in_place() {
        for (from, to) in [(44_100u32, 48_000u32), (48_000, 44_100), (96_000, 48_000), (22_050, 48_000)] {
            let at = 1000usize;
            let mut src = vec![0.0f32; 8000];
            src[at] = 1.0;
            let out = resample(&[src], from, to).unwrap();
            let peak = out[0].iter().enumerate().max_by(|a, b| a.1.abs().total_cmp(&b.1.abs())).unwrap().0;
            let want = at as f64 * to as f64 / from as f64;
            // rubato leaves up to about one input sample of residual offset
            // (measured: +0.1, 0.0, -1.0 and +0.5 for these ratios).
            let slack = 1.0 + (to as f64 / from as f64).max(1.0);
            assert!((peak as f64 - want).abs() <= slack, "{from}->{to}: peak at {peak}, want {want:.1}");
        }
    }

    #[test]
    fn wav_round_trip_via_cache() {
        let dir = std::env::temp_dir().join(format!("aitrack-engine-cache-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tone.wav");
        let spec = hound::WavSpec { channels: 1, sample_rate: 48_000, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        for i in 0..4800 {
            w.write_sample(((i % 100) as i16 - 50) * 100).unwrap();
        }
        w.finalize().unwrap();
        let cache = SampleCache::blocking();
        let a = cache.get(&path, 48_000).unwrap();
        assert_eq!(a.frames, 4800);
        assert_eq!(a.channels.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn lru_evicts_over_byte_budget() {
        let cache = SampleCache::blocking();
        // ~40 KB each (10_000 floats); budget holds two entries cleanly.
        let one = Arc::new(DecodedAudio::from_planar(vec![vec![0.0; 10_000]], 48_000));
        let sz = one.byte_size();
        cache.set_budget_bytes(sz * 2 + 1024);
        cache.insert("mem://a.wav", one);
        cache.insert("mem://b.wav", Arc::new(DecodedAudio::from_planar(vec![vec![0.0; 10_000]], 48_000)));
        assert!(cache.used_bytes() <= sz * 2 + 1024);
        // Touch `a` so `b` becomes least recently used.
        assert!(cache.get(Path::new("mem://a.wav"), 48_000).is_some());
        cache.insert("mem://c.wav", Arc::new(DecodedAudio::from_planar(vec![vec![0.0; 10_000]], 48_000)));
        assert!(cache.used_bytes() <= sz * 2 + 1024);
        let st = cache.lock();
        assert!(st.entries.contains_key(&(PathBuf::from("mem://a.wav"), 48_000)));
        assert!(!st.entries.contains_key(&(PathBuf::from("mem://b.wav"), 48_000)));
        assert!(st.entries.contains_key(&(PathBuf::from("mem://c.wav"), 48_000)));
    }

    // A 0.2 s mono clip at 22 050 Hz: silence, then a 4 ms burst of 3 kHz
    // that starts 100 ms in (its energy centroid sits at 102 ms). One copy
    // coded by LAME at 32 kbps with its gapless tag (an MP3 holds ~50 ms of
    // encoder delay before the first sample), one as AAC in an M4A.
    const CLICK_MP3: &str = concat!(
        "4944330400000000000a00000000000000000000fff370c0000000000000000000496e666f0000000f0000000a000004",
        "cb003b3b3b3b3b3b3b3b3b51515151515151515151676767676767676767677d7d7d7d7d7d7d7d7d7d92929292929292",
        "929292a8a8a8a8a8a8a8a8a8a8bebebebebebebebebebed4d4d4d4d4d4d4d4d4d4eaeaeaeaeaeaeaeaeaeaffffffffff",
        "ffffffffff000000004c617666206c616d6500000000000000000000000024030600000000000004cb4085f595000000",
        "00000000000000000000fff340c40000000348000000004c414d45342e305555555555555555555555554c414d45342e",
        "305555555555555555555555555555555555555555555555555555555555555555555555555555555555555555555555",
        "555555555555555555555555555555555555fff342c45b00000348000000005555555555555555555555555555555555",
        "55555518f2fefebebfbff45ffe78e1e2cfd4831c270830fb10144160b2ffe4003530b170304e0140c0001a0060580617",
        "00a01826002060c8790197021806901820194e586071ad758195e4fff340c4a40000034800000000b7ff0300e0143d01",
        "0790059baae0695446818d812003429c0c0c81d119058410f164163ffffff734cf0d2a030145b64057003f90bfcbc210",
        "8436b5ad6b5ad6aebfff16b56b5cd6b5ad6d6b5bfc5ad5ad70c45f82f804a00e8b0922fff342c4a30000034801400000",
        "0128003000a04f28c38403509110a51474e96d27268a1aae7d1b7c440d0340d7cb3f8340d0340cf05445fc454c414d45",
        "342e305555555555555555555555555555555555555555555555555555555555555555555555555555555555fff340c4",
        "ff1c8a2e741181b001555555555555555555555555555555555555554c414d45342e3055555555555555555555555555",
        "555555555555555555555555555555555555555555555555555555555555555555555555555555555555555555555555",
        "55555555fff342c4e716f97a9a57c1780255555555555555555555555555555555555555554c414d45342e3055555555",
        "555555555555555555555555555555555555555555555555555555555555555555555555555555555555555555555555",
        "55555555555555555555555555fff340c4a40000034800000000555555555555555555555555555555555555554c414d",
        "45342e305555555555555555555555555555555555555555555555555555555555555555555555555555555555555555",
        "555555555555555555555555555555555555555555fff342c4a300000348000000005555555555555555555555555555",
        "5555555555554c414d45342e305555555555555555555555555555555555555555555555555555555555555555555555",
        "555555555555555555555555555555555555555555555555555555555555fff340c4a400000348000000005555555555",
        "555555555555555555555555555555555555555555555555555555555555555555555555555555555555555555555555",
        "5555555555555555555555555555555555555555555555555555555555555555555555555555fff342c4a30000034800",
        "000000555555555555555555555555555555555555555555555555555555555555555555555555555555555555555555",
        "5555555555555555555555555555555555555555555555555555555555555555555555555555555555555555555555"
    );

    const CLICK_M4A: &str = concat!(
        "0000001c667479704d344120000002004d34412069736f6d69736f3200000008667265650000016a6d64617401182007",
        "0118400700e89ba43ebfafebf8c3f39f2ff7d7e21f9a77ebecfc7eff7d7ed543edfa3f5fbfea001fdecaa000ffcbfe20",
        "1f96c82100fffb2a8803212c899a018b6e900f52220000006a3fc87d1f690000728f87f1a496000049d04880b5d22b52",
        "0000001200edd04fc2fbaf6f70d0000002429a44f18919f9d6064e7910a800000000901590579593934b8312dd2dd43b",
        "a4183c2c1598f8049e943cabeb15d0b221110fb268215060c87124d2120e6c8e7b7c4b95e208722713c7afb25c151039",
        "06498f2500f6774ff42c87a8ac70d9a1b101281382758caa0b6a8707d2fc05f5b38cff8fe08032683e18032a83e94019",
        "5439d8c01828492e20060a7251e9005d22eea00c823ad164a38c8ba680001050c9401f48ac00006752664ca387000049",
        "a0cec6e45fcb768eea000000230ab6768a4572890611113c90e080000064149213305364e3e4135bc2c824ad80000000",
        "01c00118e0070118200701182007000002ee6d6f6f760000006c6d766864000000000000000000000000000056220000",
        "113a00010000010000000000000000000000000100000000000000000000000000000001000000000000000000000000",
        "000040000000000000000000000000000000000000000000000000000000000000020000023d7472616b0000005c746b",
        "686400000003000000000000000000000001000000000000113a00000000000000000000000101000000000100000000",
        "000000000000000000000001000000000000000000000000000040000000000000000000000000000024656474730000",
        "001c656c737400000000000000010000113a0000040000010000000001b56d646961000000206d646864000000000000",
        "000000000000000056220000153a55c400000000002d68646c720000000000000000736f756e00000000000000000000",
        "0000536f756e6448616e646c657200000001606d696e6600000010736d686400000000000000000000002464696e6600",
        "00001c6472656600000000000000010000000c75726c2000000001000001247374626c0000006a737473640000000000",
        "0000010000005a6d70346100000000000000010000000000000000000100100000000056220000000000366573647300",
        "00000003808080250001000480808017401500000000005dc000002ce30580808005138856e500068080800102000000",
        "207374747300000000000000020000000500000400000000010000013a0000001c737473630000000000000001000000",
        "0100000006000000010000002c7374737a00000000000000000000000600000004000000040000014e00000004000000",
        "0400000004000000147374636f00000000000000010000002c0000001a7367706401000000726f6c6c00000002000000",
        "01ffff0000001c7362677000000000726f6c6c0000000100000006000000010000003d75647461000000356d65746100",
        "0000000000002168646c7200000000000000006d6469726170706c00000000000000000000000008696c7374"
    );

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len() / 2).map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap()).collect()
    }

    /// Where the energy of the file sits, in milliseconds.
    fn centroid_ms(channels: &[Vec<f32>], rate: u32) -> f64 {
        let (mut e, mut m) = (0.0, 0.0);
        for (i, v) in channels[0].iter().enumerate() {
            e += (*v as f64).powi(2);
            m += i as f64 * (*v as f64).powi(2);
        }
        m / e / rate as f64 * 1000.0
    }

    fn decode_bytes(name: &str, hex: &str) -> (Vec<Vec<f32>>, u32) {
        let path = std::env::temp_dir().join(format!("aitrack-{}-{name}", std::process::id()));
        std::fs::write(&path, unhex(hex)).unwrap();
        let decoded = decode_file(&path);
        let _ = std::fs::remove_file(&path);
        decoded.unwrap()
    }

    /// The encoder delay and padding of an MP3 are cut: the burst lands where
    /// it was recorded (±1 ms) and the file keeps its length.
    #[test]
    fn mp3_decodes_gapless() {
        let (ch, rate) = decode_bytes("click.mp3", CLICK_MP3);
        assert_eq!(rate, 22_050);
        let at = centroid_ms(&ch, rate);
        assert!((at - 102.0).abs() < 1.0, "the burst is at {at:.2} ms, not 102");
        assert!((ch[0].len() as i64 - 4410).abs() <= 2, "{} frames", ch[0].len());
    }

    /// An M4A (AAC) plays. symphonia 0.5.5 reads its edit list but does not
    /// apply it, so the encoder's priming (1024 frames) is still in front:
    /// the burst is that late and no more.
    #[test]
    fn m4a_decodes() {
        let (ch, rate) = decode_bytes("click.m4a", CLICK_M4A);
        assert_eq!(rate, 22_050);
        assert!(ch[0].len() > 4000, "{} frames", ch[0].len());
        let late = centroid_ms(&ch, rate) - 102.0;
        assert!((0.0..=1024.0 / 22.05 + 3.0).contains(&late), "the burst is {late:.2} ms late");
    }
}
