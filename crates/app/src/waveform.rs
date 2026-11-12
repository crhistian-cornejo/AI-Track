//! Waveform peaks for drawing audio clips.
//!
//! [`WaveformCache`] decodes each audio file once, on a background thread,
//! into [`Peaks`]: a min/max pyramid per channel (64, 256, 1024… frames per
//! bucket) plus the samples themselves for short files, so a view can draw
//! anything from a whole song in a few pixels down to single samples.
//!
//! From a view:
//!
//! ```ignore
//! if let Some(peaks) = ws.waves.peaks(&source.path) {
//!     peaks.columns(None, first_frame, frames_per_px, width_px, &mut cols);
//!     // cols[i] = (min, max) in -1..=1 for pixel column i
//! }
//! ```
//!
//! `peaks` never blocks: on a miss it queues the decode and returns `None`;
//! [`WaveformCache::poll`] (the workspace's housekeeping timer) collects the
//! results and reports that the window should repaint. A file that cannot be
//! decoded is remembered as failed, so it draws flat instead of retrying.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};

/// Frames per bucket at the finest pyramid level.
pub const BASE: usize = 64;
/// Each level's buckets cover this many of the level below.
const FACTOR: usize = 4;
/// Channels kept apart (a wider file shows its first two).
const MAX_CHANNELS: usize = 2;
/// Files up to this many frames keep their samples for single-sample zoom
/// (about 43 s at 48 kHz, 8 MB in stereo).
const RAW_FRAMES: usize = 1 << 21;
/// Bytes of samples kept across all files; the files drawn longest ago give
/// theirs up first (their pyramids stay).
const RAW_BUDGET: usize = 48 << 20;
/// Files decoded at the same time.
const MAX_JOBS: usize = 2;

/// `(min, max)` of a run of frames, as 16-bit samples.
type Bucket = (i16, i16);

const EMPTY: Bucket = (i16::MAX, i16::MIN);

fn widen(a: Bucket, b: Bucket) -> Bucket {
    (a.0.min(b.0), a.1.max(b.1))
}

fn quantize(v: f32) -> i16 {
    (v.clamp(-1., 1.) * 32767.).round() as i16
}

fn unit(v: i16) -> f32 {
    f32::from(v) / 32767.
}

#[derive(Clone)]
struct Channel {
    /// `levels[k]` has one bucket per `BASE * FACTOR^k` frames.
    levels: Vec<Vec<Bucket>>,
    raw: Option<Vec<i16>>,
}

impl Channel {
    fn build(samples: &[f32], keep_raw: bool) -> Self {
        let fold = |c: &[f32]| c.iter().fold(EMPTY, |acc, v| widen(acc, (quantize(*v), quantize(*v))));
        let mut levels = vec![samples.chunks(BASE).map(fold).collect::<Vec<Bucket>>()];
        while levels.last().is_some_and(|l| l.len() > 1) {
            let next = levels[levels.len() - 1].chunks(FACTOR).map(|c| c.iter().fold(EMPTY, |acc, b| widen(acc, *b))).collect();
            levels.push(next);
        }
        Self { levels, raw: keep_raw.then(|| samples.iter().map(|v| quantize(*v)).collect()) }
    }

    /// Min/max of frames `a..b` (non-empty, inside the file).
    fn range(&self, a: usize, b: usize) -> Bucket {
        let span = b - a;
        if let Some(raw) = &self.raw
            && span < 2 * BASE
        {
            return raw[a..b].iter().fold(EMPTY, |acc, v| widen(acc, (*v, *v)));
        }
        // The coarsest level whose buckets are at most half the range, so the
        // partial buckets at both ends add little.
        let (mut k, mut size) = (0, BASE);
        while k + 1 < self.levels.len() && size * FACTOR * 2 <= span {
            k += 1;
            size *= FACTOR;
        }
        let level = &self.levels[k];
        let first = (a / size).min(level.len().saturating_sub(1));
        let last = b.div_ceil(size).clamp(first + 1, level.len().max(first + 1));
        level.get(first..last).unwrap_or(&[]).iter().fold(EMPTY, |acc, b| widen(acc, *b))
    }
}

/// The min/max pyramid of one audio file.
#[derive(Clone)]
pub struct Peaks {
    sample_rate: u32,
    frames: usize,
    channels: Vec<Channel>,
}

impl Peaks {
    /// Build from planar samples (one `Vec` per channel, as the decoder gives them).
    pub fn from_planar(planar: &[Vec<f32>], sample_rate: u32) -> Self {
        Self::build(planar, sample_rate, RAW_FRAMES)
    }

    fn build(planar: &[Vec<f32>], sample_rate: u32, raw_frames: usize) -> Self {
        let frames = planar.iter().map(Vec::len).min().unwrap_or(0);
        let keep_raw = frames <= raw_frames;
        let channels = planar.iter().take(MAX_CHANNELS).map(|c| Channel::build(&c[..frames], keep_raw)).collect();
        Self { sample_rate, frames, channels }
    }

    /// Sample rate of the file the frames are counted in.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Length of the file in frames.
    pub fn frames(&self) -> u64 {
        self.frames as u64
    }

    /// Channels drawn apart: 1 or 2.
    pub fn channels(&self) -> usize {
        self.channels.len()
    }

    /// Whether single samples are available ([`Peaks::sample`]); long files
    /// keep only the pyramid.
    pub fn has_samples(&self) -> bool {
        self.channels.first().is_some_and(|c| c.raw.is_some())
    }

    /// The smallest useful zoom in frames per pixel: 1 with samples, otherwise
    /// a quarter of the finest bucket (a bucket is then 4 px wide).
    pub fn finest(&self) -> f64 {
        if self.has_samples() { 1. } else { BASE as f64 / 4. }
    }

    fn pick(&self, channel: Option<usize>) -> &[Channel] {
        match channel {
            Some(c) => self.channels.get(c..=c).unwrap_or(&[]),
            None => &self.channels,
        }
    }

    /// `(min, max)` in -1..=1 of the frames `from..to` of one channel, or of
    /// all channels together with `None`. `None` when the range is empty or
    /// lies outside the file. Ranges shorter than two buckets are exact when
    /// the file kept its samples; longer ones round outwards to whole
    /// buckets no longer than half the range.
    pub fn range(&self, channel: Option<usize>, from: f64, to: f64) -> Option<(f32, f32)> {
        // Also false for NaN.
        if to.partial_cmp(&from) != Some(std::cmp::Ordering::Greater) || to <= 0. || from >= self.frames as f64 {
            return None;
        }
        let a = from.max(0.).floor() as usize;
        let b = (to.ceil() as usize).min(self.frames).max(a + 1);
        let found = self.pick(channel).iter().fold(EMPTY, |acc, c| widen(acc, c.range(a, b)));
        (found.0 <= found.1).then(|| (unit(found.0), unit(found.1)))
    }

    /// Fill `out` with one `(min, max)` per pixel column: column `i` covers
    /// the frames `start + i * frames_per_px ..` for `frames_per_px` frames.
    /// Columns outside the file are `(0, 0)`. Neighbouring columns are made
    /// to touch, so drawing each as a vertical bar gives an unbroken outline
    /// however far the view is zoomed in.
    #[allow(dead_code)] // The arrangement lanes draw clips with it next.
    pub fn columns(&self, channel: Option<usize>, start: f64, frames_per_px: f64, count: usize, out: &mut Vec<(f32, f32)>) {
        out.clear();
        out.reserve(count);
        let mut prev = None;
        for i in 0..count {
            let a = start + i as f64 * frames_per_px;
            let col = self.range(channel, a, a + frames_per_px);
            out.push(join(col, prev));
            prev = col;
        }
    }

    /// One sample in -1..=1 (the mean of the channels with `None`), when the
    /// file kept its samples and `frame` is inside it.
    pub fn sample(&self, channel: Option<usize>, frame: u64) -> Option<f32> {
        let picked = self.pick(channel);
        let mut sum = 0.;
        for c in picked {
            sum += unit(*c.raw.as_ref()?.get(usize::try_from(frame).ok()?)?);
        }
        (!picked.is_empty()).then(|| sum / picked.len() as f32)
    }

    fn raw_bytes(&self) -> usize {
        self.channels.iter().map(|c| c.raw.as_ref().map_or(0, |r| r.len() * 2)).sum()
    }

    fn without_samples(&self) -> Self {
        let channels = self.channels.iter().map(|c| Channel { levels: c.levels.clone(), raw: None }).collect();
        Self { sample_rate: self.sample_rate, frames: self.frames, channels }
    }
}

/// A column widened to touch the one before it (`(0, 0)` outside the file).
pub fn join(col: Option<(f32, f32)>, prev: Option<(f32, f32)>) -> (f32, f32) {
    match (col, prev) {
        (Some((lo, hi)), Some((prev_lo, prev_hi))) => (lo.min(prev_hi), hi.max(prev_lo)),
        (Some(c), None) => c,
        (None, _) => (0., 0.),
    }
}

/// Where a file is in the cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status<'a> {
    /// Nobody asked for it yet.
    Idle,
    Loading,
    Ready,
    /// It could not be decoded; the reason.
    Failed(&'a str),
}

type Decoded = Result<(Vec<Vec<f32>>, u32), String>;
type Decoder = Arc<dyn Fn(&Path) -> Decoded + Send + Sync>;

enum Entry {
    Loading,
    Ready { peaks: Arc<Peaks>, used: u64 },
    Failed(String),
}

/// Peaks of the audio files on screen, by path.
pub struct WaveformCache {
    entries: HashMap<PathBuf, Entry>,
    queue: VecDeque<PathBuf>,
    running: usize,
    tx: Sender<(PathBuf, Result<Peaks, String>)>,
    rx: Receiver<(PathBuf, Result<Peaks, String>)>,
    decoder: Decoder,
    /// Counts `peaks` calls, to know which files were drawn longest ago.
    clock: u64,
    raw_budget: usize,
}

impl Default for WaveformCache {
    fn default() -> Self {
        Self::with_decoder(Arc::new(aitrack_engine::sample_cache::decode_file))
    }
}

impl WaveformCache {
    fn with_decoder(decoder: Decoder) -> Self {
        let (tx, rx) = channel();
        Self { entries: HashMap::new(), queue: VecDeque::new(), running: 0, tx, rx, decoder, clock: 0, raw_budget: RAW_BUDGET }
    }

    /// The peaks of `path` when they are ready. A file seen for the first
    /// time is queued for decoding in the background (`None` until then);
    /// one that failed stays `None`.
    pub fn peaks(&mut self, path: &Path) -> Option<Arc<Peaks>> {
        self.clock += 1;
        match self.entries.get_mut(path) {
            Some(Entry::Ready { peaks, used }) => {
                *used = self.clock;
                Some(peaks.clone())
            }
            Some(_) => None,
            None => {
                self.entries.insert(path.to_owned(), Entry::Loading);
                self.queue.push_back(path.to_owned());
                self.pump();
                None
            }
        }
    }

    pub fn status(&self, path: &Path) -> Status<'_> {
        match self.entries.get(path) {
            None => Status::Idle,
            Some(Entry::Loading) => Status::Loading,
            Some(Entry::Ready { .. }) => Status::Ready,
            Some(Entry::Failed(why)) => Status::Failed(why),
        }
    }

    /// Forget a file so the next `peaks` call decodes it again (it changed on
    /// disk, or was relinked). A file that is being decoded right now is left
    /// alone: its result is already the fresh one.
    #[allow(dead_code)] // For relinking and "reload sample".
    pub fn forget(&mut self, path: &Path) {
        if !matches!(self.entries.get(path), Some(Entry::Loading)) {
            self.entries.remove(path);
        }
    }

    /// Housekeeping tick: true when new peaks arrived and the UI should repaint.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        while let Ok((path, result)) = self.rx.try_recv() {
            self.running = self.running.saturating_sub(1);
            changed = true;
            let entry = match result {
                Ok(peaks) => Entry::Ready { peaks: Arc::new(peaks), used: self.clock },
                Err(why) => {
                    log::warn!("waveform: cannot read {}: {why}", path.display());
                    Entry::Failed(why)
                }
            };
            self.entries.insert(path, entry);
        }
        if changed {
            self.trim();
            self.pump();
        }
        changed
    }

    /// Start queued decodes while there is a free worker.
    fn pump(&mut self) {
        while self.running < MAX_JOBS {
            let Some(path) = self.queue.pop_front() else { break };
            let (tx, decode, job_path) = (self.tx.clone(), self.decoder.clone(), path.clone());
            let job = move || {
                let work = std::panic::AssertUnwindSafe(|| {
                    let (planar, rate) = decode(&job_path)?;
                    if planar.first().is_none_or(Vec::is_empty) {
                        return Err("the file has no audio".to_owned());
                    }
                    Ok(Peaks::from_planar(&planar, rate))
                });
                let result = std::panic::catch_unwind(work).unwrap_or_else(|_| Err("the decoder crashed".to_owned()));
                let _ = tx.send((job_path, result));
            };
            match std::thread::Builder::new().name("waveform".into()).spawn(job) {
                Ok(_) => self.running += 1,
                Err(e) => {
                    self.entries.insert(path, Entry::Failed(e.to_string()));
                }
            }
        }
    }

    /// Keep the samples held for single-sample zoom inside the budget.
    fn trim(&mut self) {
        let mut total: usize = self.entries.values().map(|e| if let Entry::Ready { peaks, .. } = e { peaks.raw_bytes() } else { 0 }).sum();
        while total > self.raw_budget {
            let oldest = self
                .entries
                .values_mut()
                .filter_map(|e| match e {
                    Entry::Ready { peaks, used } if peaks.raw_bytes() > 0 => Some((*used, peaks)),
                    _ => None,
                })
                .min_by_key(|(used, _)| *used);
            let Some((_, peaks)) = oldest else { break };
            total -= peaks.raw_bytes();
            *peaks = Arc::new(peaks.without_samples());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    /// A ramp from -1 to 1 over `frames`: the min/max of any range are its ends.
    fn ramp(frames: usize) -> Vec<f32> {
        (0..frames).map(|i| -1. + 2. * i as f32 / (frames - 1) as f32).collect()
    }

    fn exact(samples: &[f32], a: usize, b: usize) -> (f32, f32) {
        samples[a..b].iter().fold((f32::MAX, f32::MIN), |(lo, hi), v| (lo.min(*v), hi.max(*v)))
    }

    const Q: f32 = 1. / 32767.;

    fn wait(cache: &mut WaveformCache, mut done: impl FnMut(&mut WaveformCache) -> bool) {
        let start = Instant::now();
        while !done(cache) {
            assert!(start.elapsed() < Duration::from_secs(10), "the decode never finished");
            std::thread::sleep(Duration::from_millis(2));
            cache.poll();
        }
    }

    #[test]
    fn pyramid_levels_shrink_by_four_down_to_one_bucket() {
        let peaks = Peaks::from_planar(&[ramp(100_000)], 48_000);
        let sizes: Vec<usize> = peaks.channels[0].levels.iter().map(Vec::len).collect();
        assert_eq!(sizes, vec![1563, 391, 98, 25, 7, 2, 1]);
        assert_eq!((peaks.frames(), peaks.sample_rate(), peaks.channels()), (100_000, 48_000, 1));
        assert!(peaks.has_samples());
        assert_eq!(peaks.finest(), 1.);
    }

    #[test]
    fn a_known_signal_gives_the_right_range_at_any_zoom() {
        let samples = ramp(100_000);
        for raw_frames in [usize::MAX, 0] {
            let peaks = Peaks::build(std::slice::from_ref(&samples), 48_000, raw_frames);
            assert_eq!(peaks.has_samples(), raw_frames > 0);
            // Ranges on bucket boundaries are exact at every level (to 16 bits).
            for size in [64, 256, 1024, 4096, 16_384] {
                for (a, b) in [(0, size), (size, 3 * size), (4 * size, 12 * size)] {
                    let (lo, hi) = peaks.range(None, a as f64, b as f64).unwrap();
                    let want = exact(&samples, a, b.min(samples.len()));
                    assert!((lo - want.0).abs() <= Q && (hi - want.1).abs() <= Q, "{a}..{b}: {lo}..{hi} is not {want:?}");
                }
            }
            // Any other range contains the true extremes and reaches at most
            // half its own length past each end.
            for (a, b) in [(10, 5_000), (333, 777), (70_001, 99_999), (0, 100_000), (64, 65), (1_000, 1_100), (12_345, 12_346)] {
                let (lo, hi) = peaks.range(None, a as f64, b as f64).unwrap();
                let want = exact(&samples, a, b);
                assert!(lo <= want.0 + Q && hi >= want.1 - Q, "{a}..{b} lost a peak");
                let slack = if raw_frames > 0 && b - a < 2 * BASE { 0 } else { ((b - a) / 2).max(BASE) };
                let wide = exact(&samples, a.saturating_sub(slack), (b + slack).min(samples.len()));
                assert!(lo >= wide.0 - Q && hi <= wide.1 + Q, "{a}..{b} reaches too far: {lo}..{hi} outside {wide:?}");
            }
        }
    }

    #[test]
    fn ranges_past_the_end_are_empty() {
        let peaks = Peaks::from_planar(&[ramp(1000)], 44_100);
        assert_eq!(peaks.range(None, 1000., 1200.), None);
        assert_eq!(peaks.range(None, 5000., 6000.), None);
        assert_eq!(peaks.range(None, -50., 0.), None);
        assert_eq!(peaks.range(None, 10., 10.), None);
        assert_eq!(peaks.range(None, 20., 10.), None);
        assert_eq!(peaks.range(None, f64::NAN, 10.), None);
        assert_eq!(peaks.range(Some(1), 0., 10.), None, "there is no second channel");
        // A range that starts inside the file is cut at its end.
        let (lo, hi) = peaks.range(None, 990., 5000.).unwrap();
        assert!(lo > 0.97 && (hi - 1.).abs() <= Q);
        let (lo, _) = peaks.range(None, -100., 2.).unwrap();
        assert!((lo + 1.).abs() <= Q);
        assert_eq!(peaks.sample(None, 1000), None);
        assert!((peaks.sample(None, 999).unwrap() - 1.).abs() <= Q);
        // An empty file has nothing anywhere.
        let empty = Peaks::from_planar(&[Vec::new()], 48_000);
        assert_eq!(empty.range(None, 0., 100.), None);
        assert_eq!(Peaks::from_planar(&[], 48_000).range(None, 0., 100.), None);
    }

    #[test]
    fn columns_cover_the_file_and_touch_each_other() {
        // A square wave of period 100: single-sample columns alternate.
        let square: Vec<f32> = (0..10_000).map(|i| if i % 100 < 50 { 0.5 } else { -0.5 }).collect();
        let peaks = Peaks::from_planar(std::slice::from_ref(&square), 48_000);
        let mut cols = Vec::new();
        // Zoomed out: 100 frames per pixel, every column spans a whole period.
        peaks.columns(None, 0., 100., 120, &mut cols);
        assert_eq!(cols.len(), 120);
        for c in &cols[..100] {
            assert!((c.0 + 0.5).abs() < 1e-3 && (c.1 - 0.5).abs() < 1e-3);
        }
        assert!(cols[100..].iter().all(|c| *c == (0., 0.)), "nothing past the end");
        // Zoomed in: 10 frames per pixel. The column where the wave jumps
        // down reaches back up to the one before it.
        peaks.columns(None, 0., 10., 20, &mut cols);
        assert!((cols[4].0 - 0.5).abs() < 1e-3 && (cols[4].1 - 0.5).abs() < 1e-3);
        assert!((cols[5].0 + 0.5).abs() < 1e-3 && (cols[5].1 - 0.5).abs() < 1e-3, "joined to its neighbour: {:?}", cols[5]);
        assert!((cols[6].0 + 0.5).abs() < 1e-3 && (cols[6].1 + 0.5).abs() < 1e-3);
        // Before the file starts there is nothing.
        peaks.columns(None, -50., 10., 6, &mut cols);
        assert_eq!(cols[..5], [(0., 0.); 5]);
        assert!((cols[5].1 - 0.5).abs() < 1e-3);
    }

    #[test]
    fn channels_are_kept_apart_and_merged_on_request() {
        let left = vec![0.25f32; 4096];
        let right = vec![-0.75f32; 4096];
        let peaks = Peaks::from_planar(&[left, right, vec![1.; 4096]], 48_000);
        assert_eq!(peaks.channels(), 2, "a third channel is not drawn");
        let near = |a: (f32, f32), b: (f32, f32)| (a.0 - b.0).abs() < 1e-3 && (a.1 - b.1).abs() < 1e-3;
        for (a, b) in [(0., 10.), (0., 4096.), (100., 2000.)] {
            assert!(near(peaks.range(Some(0), a, b).unwrap(), (0.25, 0.25)));
            assert!(near(peaks.range(Some(1), a, b).unwrap(), (-0.75, -0.75)));
            assert!(near(peaks.range(None, a, b).unwrap(), (-0.75, 0.25)));
        }
        assert!((peaks.sample(Some(1), 7).unwrap() + 0.75).abs() < 1e-3);
        assert!((peaks.sample(None, 7).unwrap() + 0.25).abs() < 1e-3);
    }

    #[test]
    fn long_files_keep_only_the_pyramid() {
        let samples = ramp(10_000);
        let peaks = Peaks::build(&[samples], 48_000, 5_000);
        assert!(!peaks.has_samples());
        assert_eq!(peaks.sample(None, 10), None);
        assert_eq!(peaks.finest(), 16.);
        // Still answers at the finest level when zoomed in past it.
        let (lo, hi) = peaks.range(None, 100., 101.).unwrap();
        assert!(lo < hi && lo >= -1. && hi < -0.97);
    }

    #[test]
    fn a_wav_is_decoded_once_in_the_background() {
        let dir = std::env::temp_dir().join(format!("aitrack-waveform-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tone.wav");
        let spec = hound::WavSpec { channels: 2, sample_rate: 44_100, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        for i in 0..4410 {
            let v = ((i as f32 / 44.1).sin() * 16_000.) as i16;
            w.write_sample(v).unwrap();
            w.write_sample(-v).unwrap();
        }
        w.finalize().unwrap();

        let mut cache = WaveformCache::default();
        assert_eq!(cache.status(&path), Status::Idle);
        assert!(cache.peaks(&path).is_none(), "never blocks");
        assert_eq!(cache.status(&path), Status::Loading);
        assert!(!cache.poll() || cache.status(&path) == Status::Ready);
        wait(&mut cache, |c| c.status(&path) == Status::Ready);
        let peaks = cache.peaks(&path).expect("ready");
        assert_eq!((peaks.frames(), peaks.sample_rate(), peaks.channels()), (4410, 44_100, 2));
        let (lo, hi) = peaks.range(None, 0., 4410.).unwrap();
        assert!((hi - 16_000. / 32_768.).abs() < 0.01 && (lo + 16_000. / 32_768.).abs() < 0.01);
        assert!(!cache.poll(), "nothing new");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_same_file_is_never_decoded_twice_and_failures_are_final() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let mut cache = WaveformCache::with_decoder(Arc::new(move |path: &Path| {
            counter.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(20));
            match path.to_str() {
                Some("good.wav") => Ok((vec![vec![0.5; 1000]], 48_000)),
                Some("empty.wav") => Ok((vec![Vec::new()], 48_000)),
                Some("crash.wav") => panic!("decoder bug"),
                _ => Err("no such file".to_owned()),
            }
        }));
        let (good, bad) = (Path::new("good.wav"), Path::new("missing.wav"));
        // Asking again while it loads (every frame does) starts nothing new.
        for _ in 0..50 {
            assert!(cache.peaks(good).is_none());
            assert!(cache.peaks(bad).is_none());
        }
        cache.forget(good);
        assert_eq!(cache.status(good), Status::Loading, "a running decode is not forgotten");
        wait(&mut cache, |c| c.status(good) == Status::Ready && matches!(c.status(bad), Status::Failed(_)));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(cache.status(bad), Status::Failed("no such file"));
        // A failed file is not retried, however often it is drawn.
        for _ in 0..50 {
            assert!(cache.peaks(bad).is_none());
            cache.poll();
        }
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(cache.peaks(good).is_some());
        // Empty files and decoder panics fail too, and do not jam the queue.
        let (empty, crash) = (Path::new("empty.wav"), Path::new("crash.wav"));
        assert!(cache.peaks(empty).is_none() && cache.peaks(crash).is_none());
        wait(&mut cache, |c| matches!(c.status(empty), Status::Failed(_)) && matches!(c.status(crash), Status::Failed(_)));
        assert_eq!(cache.status(crash), Status::Failed("the decoder crashed"));
        assert_eq!(cache.running, 0);
        // Forgetting a file decodes it again on the next request.
        cache.forget(bad);
        assert_eq!(cache.status(bad), Status::Idle);
        assert!(cache.peaks(bad).is_none());
        wait(&mut cache, |c| matches!(c.status(bad), Status::Failed(_)));
        assert_eq!(calls.load(Ordering::SeqCst), 5);
    }

    #[test]
    fn more_files_than_workers_wait_their_turn() {
        let running = Arc::new(AtomicUsize::new(0));
        let most = Arc::new(AtomicUsize::new(0));
        let (r, m) = (running.clone(), most.clone());
        let mut cache = WaveformCache::with_decoder(Arc::new(move |_: &Path| {
            let now = r.fetch_add(1, Ordering::SeqCst) + 1;
            m.fetch_max(now, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(15));
            r.fetch_sub(1, Ordering::SeqCst);
            Ok((vec![vec![0.1; 500]], 48_000))
        }));
        let paths: Vec<PathBuf> = (0..7).map(|i| PathBuf::from(format!("{i}.wav"))).collect();
        for p in &paths {
            assert!(cache.peaks(p).is_none());
        }
        wait(&mut cache, |c| paths.iter().all(|p| c.status(p) == Status::Ready));
        assert!(most.load(Ordering::SeqCst) <= MAX_JOBS);
    }

    #[test]
    fn samples_of_files_drawn_long_ago_are_dropped_first() {
        let mut cache = WaveformCache::with_decoder(Arc::new(|_: &Path| Ok((vec![vec![0.3; 1000]], 48_000))));
        // Room for two files' samples (2000 bytes each).
        cache.raw_budget = 4500;
        let (a, b, c) = (Path::new("a.wav"), Path::new("b.wav"), Path::new("c.wav"));
        for p in [a, b] {
            cache.peaks(p);
        }
        wait(&mut cache, |cache| cache.status(a) == Status::Ready && cache.status(b) == Status::Ready);
        // `a` is on screen again, then a third file arrives.
        assert!(cache.peaks(b).unwrap().has_samples());
        assert!(cache.peaks(a).unwrap().has_samples());
        cache.peaks(c);
        wait(&mut cache, |cache| cache.status(c) == Status::Ready);
        assert!(cache.peaks(a).unwrap().has_samples());
        assert!(cache.peaks(c).unwrap().has_samples());
        let old = cache.peaks(b).unwrap();
        assert!(!old.has_samples(), "the file drawn longest ago gave its samples up");
        assert!(old.range(None, 0., 1000.).is_some(), "its pyramid stays");
    }
}
