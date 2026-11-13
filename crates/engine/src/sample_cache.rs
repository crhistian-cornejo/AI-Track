//! Decoded audio for audio clips, loaded off the audio thread.
//!
//! Files are decoded (hound for WAV, symphonia for everything else),
//! resampled with rubato to the device rate and kept fully in memory behind
//! an `Arc`, shared by every clip that uses them. Disk streaming for very
//! long files is a later step (`creek`); the [`DecodedAudio`] interface is
//! what the clip player reads, so a streamed source can replace it.

use crossbeam_channel::{unbounded, Sender};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Planar `f32` audio at `sample_rate`.
#[derive(Debug)]
pub struct DecodedAudio {
    pub channels: Vec<Vec<f32>>,
    pub sample_rate: u32,
    /// Rate of the file before resampling.
    pub source_rate: u32,
    pub frames: usize,
}

impl DecodedAudio {
    pub fn from_planar(channels: Vec<Vec<f32>>, sample_rate: u32) -> Self {
        let frames = channels.iter().map(Vec::len).min().unwrap_or(0);
        init_interpolator();
        Self { channels, sample_rate, source_rate: sample_rate, frames }
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
        self.channels[ch.min(self.channels.len() - 1)][index as usize]
    }

    /// Windowed-sinc interpolation at a fractional frame; 0 outside the data.
    ///
    /// Linear interpolation dulls the top octave (−3 dB at a quarter of the
    /// sample rate at half-sample offsets), so anything read at a fractional
    /// position goes through a 16-tap Kaiser-windowed sinc instead. The table
    /// is built once off the audio thread (see [`init_interpolator`]).
    #[inline]
    pub fn sample(&self, ch: usize, pos: f64) -> f32 {
        if self.channels.is_empty() || pos <= -(SINC_HALF as f64) || pos >= (self.frames + SINC_HALF) as f64 {
            return 0.0;
        }
        let data = &self.channels[ch.min(self.channels.len() - 1)];
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
        if first >= 0 && (first as usize + SINC_TAPS) <= self.frames {
            let window = &data[first as usize..first as usize + SINC_TAPS];
            for j in 0..SINC_TAPS {
                acc += window[j] * (row0[j] + (row1[j] - row0[j]) * blend);
            }
        } else {
            for j in 0..SINC_TAPS {
                let idx = first + j as i64;
                if idx >= 0 && (idx as usize) < self.frames {
                    acc += data[idx as usize] * (row0[j] + (row1[j] - row0[j]) * blend);
                }
            }
        }
        acc
    }
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
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
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
/// and the filter delay is removed, so the result is time-aligned.
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
    let delay = rs.output_delay();
    let expected = (frames as u64 * to as u64).div_ceil(from as u64) as usize;
    let mut out = vec![Vec::with_capacity(expected + delay + chunk); channels.len()];
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
    while out[0].len() < expected + delay {
        let res = rs.process_partial::<&[f32]>(None, None).map_err(|e| e.to_string())?;
        if res[0].is_empty() {
            break;
        }
        for (o, r) in out.iter_mut().zip(res) {
            o.extend_from_slice(&r);
        }
    }
    for o in &mut out {
        o.drain(..delay.min(o.len()));
        o.truncate(expected);
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

/// Default decoded sample cache budget (512 MB) before LRU eviction.
pub const DEFAULT_CACHE_BUDGET_BYTES: usize = 512 * 1024 * 1024;

enum Entry {
    Loading,
    Ready { audio: Arc<DecodedAudio>, used: u64 },
    Failed(String),
}

type Key = (PathBuf, u32);

#[derive(Default)]
struct CacheState {
    entries: HashMap<Key, Entry>,
    clock: u64,
}

impl CacheState {
    fn total_bytes(&self) -> usize {
        self.entries
            .values()
            .filter_map(|e| match e {
                Entry::Ready { audio, .. } => Some(audio.byte_size()),
                _ => None,
            })
            .sum()
    }

    fn trim(&mut self, budget: usize) {
        let mut total = self.total_bytes();
        while total > budget {
            // Prefer evicting entries no compiled graph currently holds.
            let candidate = self
                .entries
                .iter()
                .filter_map(|(k, e)| match e {
                    Entry::Ready { audio, used } => Some((Arc::strong_count(audio) > 1, *used, audio.byte_size(), k.clone())),
                    _ => None,
                })
                .min_by_key(|(in_use, used, _, _)| (*in_use, *used));
            let Some((_, _, sz, key)) = candidate else { break };
            self.entries.remove(&key);
            total = total.saturating_sub(sz);
        }
    }
}

/// Shared cache of decoded files keyed by `(path, rate)`, with a background
/// loader thread and an LRU memory budget. Control-thread / UI side only.
#[derive(Clone)]
pub struct SampleCache {
    state: Arc<Mutex<CacheState>>,
    budget: Arc<std::sync::atomic::AtomicUsize>,
    jobs: Option<Sender<Key>>,
}

impl SampleCache {
    /// A cache whose misses are loaded on a background thread; `on_loaded`
    /// runs (on that thread) after each file finishes, so the engine can
    /// recompile.
    pub fn with_loader(on_loaded: impl Fn() + Send + 'static) -> Self {
        let state: Arc<Mutex<CacheState>> = Arc::default();
        let budget = Arc::new(std::sync::atomic::AtomicUsize::new(DEFAULT_CACHE_BUDGET_BYTES));
        let (tx, rx) = unbounded::<Key>();
        let map = state.clone();
        let budget_bg = budget.clone();
        let spawned = std::thread::Builder::new().name("aitrack-sample-loader".into()).spawn(move || {
            while let Ok(key) = rx.recv() {
                let entry = match load_audio(&key.0, key.1) {
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
                on_loaded();
            }
        });
        if let Err(e) = spawned {
            log::error!("sample loader thread failed to start: {e}");
        }
        Self { state, budget, jobs: Some(tx) }
    }

    /// A cache without a loader: misses are decoded synchronously (offline
    /// render, tests).
    pub fn blocking() -> Self {
        Self {
            state: Arc::default(),
            budget: Arc::new(std::sync::atomic::AtomicUsize::new(DEFAULT_CACHE_BUDGET_BYTES)),
            jobs: None,
        }
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

    pub fn used_bytes(&self) -> usize {
        self.lock().total_bytes()
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
        let res = match load_audio(path, rate) {
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
        self.lock().entries.values().any(|e| matches!(e, Entry::Loading))
    }

    /// Forget everything (e.g. after a device rate change the old entries
    /// are useless).
    pub fn clear(&self) {
        self.lock().entries.retain(|_, e| matches!(e, Entry::Loading));
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
}
