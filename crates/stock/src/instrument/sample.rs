//! In-memory and windowed disk-streamed audio samples for the sampler and
//! drum sampler: a WAV reader (via `hound`) and a cubic-Hermite reader for
//! fractional playback.
//!
//! Loading and decoding run on the control thread (`load_state`, explicit
//! `load_*` calls), and large WAV files (> [`DEFAULT_STREAM_THRESHOLD_FRAMES`])
//! open with only a small RAM preroll + fixed pre-allocated page ring fed on
//! a background reader thread. The audio thread never allocates, locks, or blocks.

use aitrack_dsp::delay::hermite;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// Default threshold (~10 s @ 48 kHz) above which a zone opens in streamed mode.
pub const DEFAULT_STREAM_THRESHOLD_FRAMES: usize = 48_000 * 10;
/// Frames held immediately in RAM for zero-latency note-on attack (~340 ms @ 48 kHz).
pub const DEFAULT_PREROLL_FRAMES: usize = 16_384;
/// Frames per disk-stream page.
pub const STREAM_PAGE_FRAMES: usize = 16_384;
/// Pre-allocated page slots per streamed zone (zero allocations on audio thread).
const STREAM_SLOTS: usize = 6;

/// Planar audio data (1 or 2 channels).
#[derive(Clone, Debug)]
pub struct SampleData {
    channels: usize,
    frames: usize,
    data: Vec<f32>,
    pub sample_rate: f64,
    /// Where it came from, for state saving.
    pub path: Option<String>,
}

impl SampleData {
    /// From interleaved samples. More than two channels keep the first two.
    pub fn from_interleaved(data: &[f32], channels: usize, sample_rate: f64) -> Self {
        let in_ch = channels.max(1);
        let ch = in_ch.min(2);
        let frames = data.len() / in_ch;
        let mut planar = vec![0.0; frames * ch];
        for f in 0..frames {
            for c in 0..ch {
                let v = data[f * in_ch + c];
                planar[c * frames + f] = if v.is_finite() { v } else { 0.0 };
            }
        }
        Self { channels: ch, frames, data: planar, sample_rate: sample_rate.max(1.0), path: None }
    }

    pub fn frames(&self) -> usize {
        self.frames
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    pub fn duration_secs(&self) -> f64 {
        self.frames as f64 / self.sample_rate
    }

    #[inline]
    fn at(&self, ch: usize, i: isize) -> f32 {
        if i < 0 || i as usize >= self.frames {
            0.0
        } else {
            self.data[ch * self.frames + i as usize]
        }
    }

    /// Cubic Hermite read at fractional frame `pos` (zero outside the data).
    /// `ch` ≥ channels reads the last channel (mono fans out).
    #[inline]
    pub fn read(&self, ch: usize, pos: f64) -> f32 {
        let ch = ch.min(self.channels - 1);
        let i = pos.floor();
        let t = (pos - i) as f32;
        let i = i as isize;
        hermite(t, self.at(ch, i - 1), self.at(ch, i), self.at(ch, i + 1), self.at(ch, i + 2))
    }

    /// Peak absolute value (for normalisation / display).
    pub fn peak(&self) -> f32 {
        self.data.iter().fold(0.0f32, |m, v| m.max(v.abs()))
    }
}

/// Decode a WAV file (PCM 8/16/24/32-bit or 32-bit float).
pub fn load_wav(path: &str) -> Result<SampleData, String> {
    let mut reader = hound::WavReader::open(path).map_err(|e| format!("{path}: {e}"))?;
    let spec = reader.spec();
    let data: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().map(|s| s.unwrap_or(0.0)).collect(),
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample.clamp(1, 32) - 1)) as f32;
            reader.samples::<i32>().map(|s| s.unwrap_or(0) as f32 * scale).collect()
        }
    };
    if data.is_empty() {
        return Err(format!("{path}: no audio"));
    }
    let mut s = SampleData::from_interleaved(&data, spec.channels as usize, spec.sample_rate as f64);
    s.path = Some(path.to_string());
    Ok(s)
}

fn read_wav_range(reader: &mut hound::WavReader<std::io::BufReader<std::fs::File>>, start_frame: usize, max_frames: usize) -> SampleData {
    let spec = reader.spec();
    let in_ch = spec.channels.max(1) as usize;
    let out_ch = in_ch.min(2);
    let _ = reader.seek(start_frame as u32);
    let want_samples = max_frames * in_ch;
    let mut planar = vec![0.0f32; max_frames * out_ch];
    let mut read_frames = 0usize;
    match spec.sample_format {
        hound::SampleFormat::Float => {
            for (idx, s) in reader.samples::<f32>().take(want_samples).enumerate() {
                let f = idx / in_ch;
                let c = idx % in_ch;
                if c < out_ch {
                    let v = s.unwrap_or(0.0);
                    planar[c * max_frames + f] = if v.is_finite() { v } else { 0.0 };
                }
                if c + 1 == in_ch {
                    read_frames = f + 1;
                }
            }
        }
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample.clamp(1, 32) - 1)) as f32;
            for (idx, s) in reader.samples::<i32>().take(want_samples).enumerate() {
                let f = idx / in_ch;
                let c = idx % in_ch;
                if c < out_ch {
                    let v = s.unwrap_or(0) as f32 * scale;
                    planar[c * max_frames + f] = if v.is_finite() { v } else { 0.0 };
                }
                if c + 1 == in_ch {
                    read_frames = f + 1;
                }
            }
        }
    }
    if read_frames == 0 {
        return SampleData { channels: out_ch, frames: 0, data: Vec::new(), sample_rate: spec.sample_rate as f64, path: None };
    }
    // Trim planar buffers if fewer frames than max_frames were at EOF.
    if read_frames < max_frames {
        let mut tight = vec![0.0f32; read_frames * out_ch];
        for c in 0..out_ch {
            tight[c * read_frames..(c + 1) * read_frames].copy_from_slice(&planar[c * max_frames..c * max_frames + read_frames]);
        }
        planar = tight;
    }
    SampleData { channels: out_ch, frames: read_frames, data: planar, sample_rate: spec.sample_rate.max(1) as f64, path: None }
}

/// Pre-allocated page slot shared between the background disk reader and audio thread.
struct StreamSlot {
    /// Page index currently resident, or `u32::MAX` if free / loading.
    page: AtomicU32,
    frames: AtomicU32,
    /// Fixed capacity `channels * STREAM_PAGE_FRAMES`. Written only when `page == u32::MAX`.
    data: std::cell::UnsafeCell<Vec<f32>>,
}

// SAFETY: `page` acts as the single-writer/multiple-reader acquire/release gate:
// the worker sets `page = u32::MAX` (invalidating the slot), writes `data` +
// `frames`, then publishes `page.store(idx, Ordering::Release)`. The audio thread
// only reads `data` after observing a matching `page` with `Ordering::Acquire`.
unsafe impl Sync for StreamSlot {}

/// Shared streaming state for one large WAV file.
pub struct StreamBacking {
    path: PathBuf,
    channels: usize,
    total_frames: usize,
    pub sample_rate: f64,
    preroll: SampleData,
    slots: [StreamSlot; STREAM_SLOTS],
    next_slot: AtomicU32,
    want_page: AtomicU32,
}

impl StreamBacking {
    /// Load only the first `preroll_frames` into RAM when `total_frames > threshold`;
    /// otherwise decode the entire file into `preroll`.
    pub fn open(path: &str, threshold_frames: usize, preroll_frames: usize) -> Result<Arc<Self>, String> {
        let mut reader = hound::WavReader::open(path).map_err(|e| format!("{path}: {e}"))?;
        let spec = reader.spec();
        let in_ch = spec.channels.max(1) as usize;
        let out_ch = in_ch.min(2);
        let total_frames = reader.duration() as usize;
        if total_frames == 0 {
            return Err(format!("{path}: no audio"));
        }
        let sr = spec.sample_rate.max(1) as f64;
        if total_frames <= threshold_frames {
            let mut s = load_wav(path)?;
            s.path = Some(path.to_string());
            return Ok(Arc::new(Self {
                path: PathBuf::from(path),
                channels: s.channels(),
                total_frames: s.frames(),
                sample_rate: s.sample_rate,
                preroll: s,
                slots: std::array::from_fn(|_| StreamSlot {
                    page: AtomicU32::new(u32::MAX),
                    frames: AtomicU32::new(0),
                    data: std::cell::UnsafeCell::new(Vec::new()),
                }),
                next_slot: AtomicU32::new(0),
                want_page: AtomicU32::new(u32::MAX),
            }));
        }
        let mut preroll = read_wav_range(&mut reader, 0, preroll_frames.min(total_frames));
        preroll.path = Some(path.to_string());
        let cap = out_ch * STREAM_PAGE_FRAMES;
        Ok(Arc::new(Self {
            path: PathBuf::from(path),
            channels: out_ch,
            total_frames,
            sample_rate: sr,
            preroll,
            slots: std::array::from_fn(|_| StreamSlot {
                page: AtomicU32::new(u32::MAX),
                frames: AtomicU32::new(0),
                data: std::cell::UnsafeCell::new(vec![0.0; cap]),
            }),
            next_slot: AtomicU32::new(0),
            want_page: AtomicU32::new(u32::MAX),
        }))
    }

    pub fn from_in_memory(data: SampleData) -> Arc<Self> {
        let path = PathBuf::from(data.path.as_deref().unwrap_or(""));
        let channels = data.channels();
        let total_frames = data.frames();
        let sample_rate = data.sample_rate;
        Arc::new(Self {
            path,
            channels,
            total_frames,
            sample_rate,
            preroll: data,
            slots: std::array::from_fn(|_| StreamSlot {
                page: AtomicU32::new(u32::MAX),
                frames: AtomicU32::new(0),
                data: std::cell::UnsafeCell::new(Vec::new()),
            }),
            next_slot: AtomicU32::new(0),
            want_page: AtomicU32::new(u32::MAX),
        })
    }

    pub fn frames(&self) -> usize {
        self.total_frames
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    pub fn is_streamed(&self) -> bool {
        self.preroll.frames() < self.total_frames
    }

    pub fn preroll_frames(&self) -> usize {
        self.preroll.frames()
    }

    pub fn sample_rate(&self) -> f64 {
        self.sample_rate
    }

    /// Fill `page_idx` synchronously (called from the background worker thread or tests).
    pub fn load_page_sync(&self, page_idx: u32) -> bool {
        if self.slots.iter().any(|s| s.page.load(Ordering::Acquire) == page_idx) {
            return true;
        }
        let start_frame = page_idx as usize * STREAM_PAGE_FRAMES;
        if start_frame >= self.total_frames {
            return false;
        }
        let Ok(mut reader) = hound::WavReader::open(&self.path) else {
            return false;
        };
        let chunk = read_wav_range(&mut reader, start_frame, STREAM_PAGE_FRAMES);
        let slot_idx = self.next_slot.fetch_add(1, Ordering::Relaxed) as usize % STREAM_SLOTS;
        let slot = &self.slots[slot_idx];
        slot.page.store(u32::MAX, Ordering::Release);
        std::sync::atomic::fence(Ordering::SeqCst);
        // SAFETY: `slot.page == u32::MAX` prevents any audio-thread reader from
        // observing this page index until we store `page_idx` with `Release`.
        let buf = unsafe { &mut *slot.data.get() };
        if buf.len() < chunk.data.len() {
            // Only runs on the background/control thread if capacity wasn't reserved.
            buf.resize(chunk.data.len(), 0.0);
        }
        buf[..chunk.data.len()].copy_from_slice(&chunk.data);
        slot.frames.store(chunk.frames as u32, Ordering::Release);
        slot.page.store(page_idx, Ordering::Release);
        true
    }

    #[inline]
    fn at(&self, ch: usize, i: isize) -> f32 {
        if i < 0 {
            return 0.0;
        }
        let idx = i as usize;
        if idx >= self.total_frames {
            return 0.0;
        }
        if idx < self.preroll.frames() {
            return self.preroll.at(ch, i);
        }
        let page = (idx / STREAM_PAGE_FRAMES) as u32;
        let offset = idx % STREAM_PAGE_FRAMES;
        let next = page + 1;
        let next_start = (next as usize) * STREAM_PAGE_FRAMES;
        for slot in &self.slots {
            if slot.page.load(Ordering::Acquire) == page {
                let len = slot.frames.load(Ordering::Acquire) as usize;
                // Request lookahead for the next page if not already cached.
                if next_start < self.total_frames && !self.slots.iter().any(|s| s.page.load(Ordering::Relaxed) == next) {
                    let _ = self.want_page.compare_exchange(u32::MAX, next, Ordering::Relaxed, Ordering::Relaxed);
                }
                if offset < len {
                    // SAFETY: `slot.page` stayed `== page` during `load(Acquire)` and worker
                    // does not overwrite the oldest slot for `STREAM_SLOTS` pages.
                    let buf = unsafe { &*slot.data.get() };
                    return buf.get(ch * len + offset).copied().unwrap_or(0.0);
                }
                return 0.0;
            }
        }
        self.want_page.store(page, Ordering::Relaxed);
        0.0
    }

    /// Audio-thread cubic Hermite read (zero locks, zero allocations).
    #[inline]
    pub fn read(&self, ch: usize, pos: f64) -> f32 {
        if !self.is_streamed() {
            return self.preroll.read(ch, pos);
        }
        let ch = ch.min(self.channels - 1);
        let i = pos.floor();
        let t = (pos - i) as f32;
        let i = i as isize;
        hermite(t, self.at(ch, i - 1), self.at(ch, i), self.at(ch, i + 1), self.at(ch, i + 2))
    }

    /// Service any pending page request (called by worker thread).
    pub fn service_pending(&self) {
        let p = self.want_page.swap(u32::MAX, Ordering::AcqRel);
        if p != u32::MAX {
            self.load_page_sync(p);
        }
    }
}

/// Shared background disk-streaming worker servicing a list of `StreamBacking`s.
pub struct StreamWorker {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl StreamWorker {
    pub fn spawn(streams: Vec<Arc<StreamBacking>>) -> Option<Self> {
        let streamed: Vec<Arc<StreamBacking>> = streams.into_iter().filter(|s| s.is_streamed()).collect();
        if streamed.is_empty() {
            return None;
        }
        // Pre-warm page after preroll before returning so playback starts with lookahead ready.
        for s in &streamed {
            let next_p = (s.preroll_frames() / STREAM_PAGE_FRAMES) as u32;
            if (next_p as usize) * STREAM_PAGE_FRAMES < s.frames() {
                s.load_page_sync(next_p);
            }
        }
        let stop = Arc::new(AtomicBool::new(false));
        let stop_bg = stop.clone();
        let handle = thread::Builder::new()
            .name("sfz-stream".into())
            .spawn(move || {
                while !stop_bg.load(Ordering::Relaxed) {
                    let mut did = false;
                    for s in &streamed {
                        let p = s.want_page.swap(u32::MAX, Ordering::AcqRel);
                        if p != u32::MAX {
                            s.load_page_sync(p);
                            did = true;
                        }
                    }
                    if !did {
                        thread::sleep(Duration::from_millis(2));
                    }
                }
            })
            .ok();
        Some(Self { stop, handle })
    }
}

impl Drop for StreamWorker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hermite_reads_between_samples() {
        let s = SampleData::from_interleaved(&[0.0, 1.0, 2.0, 3.0, 4.0], 1, 48_000.0);
        assert!((s.read(0, 2.5) - 2.5).abs() < 1e-6);
        assert_eq!(s.read(0, -5.0), 0.0);
        assert_eq!(s.read(1, 1.0), 1.0); // mono fans out
    }

    #[test]
    fn wav_round_trip() {
        let dir = std::env::temp_dir().join(format!("aitrack-wav-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for (bits, fmt) in [(16u16, hound::SampleFormat::Int), (24, hound::SampleFormat::Int), (32, hound::SampleFormat::Float)] {
            let path = dir.join(format!("t{bits}.wav"));
            let spec = hound::WavSpec { channels: 2, sample_rate: 44_100, bits_per_sample: bits, sample_format: fmt };
            let mut w = hound::WavWriter::create(&path, spec).unwrap();
            for i in 0..100 {
                let v = (i as f32 / 100.0) * 0.5;
                match fmt {
                    hound::SampleFormat::Float => {
                        w.write_sample(v).unwrap();
                        w.write_sample(-v).unwrap();
                    }
                    hound::SampleFormat::Int => {
                        let m = ((1i64 << (bits - 1)) - 1) as f32;
                        w.write_sample((v * m) as i32).unwrap();
                        w.write_sample((-v * m) as i32).unwrap();
                    }
                }
            }
            w.finalize().unwrap();
            let s = load_wav(path.to_str().unwrap()).unwrap();
            assert_eq!(s.frames(), 100);
            assert_eq!(s.channels(), 2);
            assert_eq!(s.sample_rate, 44_100.0);
            assert!((s.read(0, 50.0) - 0.25).abs() < 1e-3, "{bits}");
            assert!((s.read(1, 50.0) + 0.25).abs() < 1e-3);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
