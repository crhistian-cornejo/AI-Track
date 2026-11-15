//! A small preview voice for the browser: plays one file next to the
//! engine's stream, on the same output device.
//!
//! Everything slow happens on one worker thread (`aitrack-preview`): opening
//! the device, decoding the file and freeing finished voices. The UI thread
//! only sends a request. The file is decoded at its own rate and read by the
//! callback through the engine's interpolator at `ratio × file rate / stream
//! rate`. Files up to 30 s are resampled to the stream rate with the clips'
//! own converter (`sample_cache::resample`) so the browser sounds like the
//! imported sample; longer ones are read at their own rate so playback
//! starts as soon as they are decoded. A 256-frame buffer keeps the
//! click-to-sound time short. The callback swaps voices from a wait-free queue and hands
//! the old one back on another, so it never allocates or frees.

use crate::bridge::SyncedPreview;
use aitrack_dsp::warp::{WarpAlgorithm, warp_audio};
use aitrack_engine::sample_cache::{self, DecodedAudio};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use std::path::{Path, PathBuf};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Unity: the browser plays the sample at the level its clip will have
/// (scaled by the preview volume, which starts at unity).
const GAIN: f32 = 1.0;
/// A looping preview runs this long at most, then stops.
const LOOP_SECONDS: usize = 600;
/// Rendered previews kept for switching back and forth (a loop is a few MB).
const RENDER_KEEP: usize = 8;
/// Most audio the kept renders hold in all (a 30 s stereo render at 48 kHz is 11.5 MB).
const RENDER_BUDGET_BYTES: usize = 48 << 20;
/// Longest preview.
const MAX_SECONDS: usize = 30;
/// Frames per callback asked of the device (clamped to what it supports).
const BUFFER: u32 = 256;

/// One file playing: read position in file frames, advanced by `step`.
struct Voice {
    audio: Arc<DecodedAudio>,
    pos: f64,
    step: f64,
    reverse: bool,
    /// Output frames left before the preview stops.
    left: usize,
    /// Silent output frames before it starts (it enters on a bar of the running song).
    delay: usize,
    /// Starts over at the end, until `left` runs out (loops).
    looped: bool,
    /// The preview level, 0..=1 as `f32` bits; read per frame, set from the UI.
    volume: Arc<AtomicU32>,
}

impl Voice {
    fn new(audio: DecodedAudio, stream_rate: u32, ratio: f64, reverse: bool) -> Self {
        let ratio = if ratio.is_finite() && ratio > 0. { ratio } else { 1. };
        let step = ratio * audio.sample_rate as f64 / stream_rate.max(1) as f64;
        Self { audio: Arc::new(audio), pos: 0., step, reverse, left: stream_rate as usize * MAX_SECONDS, delay: 0, looped: false, volume: Arc::new(AtomicU32::new(1f32.to_bits())) }
    }

    /// A voice for a rendered preview: starts after `delay` frames, loops when asked, at the shared volume.
    fn synced(audio: Arc<DecodedAudio>, stream_rate: u32, looped: bool, delay: usize, volume: Arc<AtomicU32>) -> Self {
        let step = audio.sample_rate as f64 / stream_rate.max(1) as f64;
        let seconds = if looped { LOOP_SECONDS } else { MAX_SECONDS };
        Self { audio, pos: 0., step, reverse: false, left: stream_rate as usize * seconds + delay, delay, looped, volume }
    }

    /// The next stereo frame, or `None` at the end. Real-time safe.
    #[inline]
    fn next(&mut self) -> Option<(f32, f32)> {
        let a = &*self.audio;
        let last = a.frames as f64 - 1.;
        if self.left == 0 || a.channels.is_empty() {
            return None;
        }
        if self.delay > 0 {
            self.delay -= 1;
            self.left -= 1;
            return Some((0., 0.));
        }
        if self.pos > last {
            if !self.looped || a.frames == 0 {
                return None;
            }
            // Round again, keeping the fraction so the loop does not drift.
            self.pos -= a.frames as f64;
        }
        let gain = GAIN * f32::from_bits(self.volume.load(Ordering::Relaxed));
        let p = if self.reverse { last - self.pos } else { self.pos };
        let right = if a.channels.len() >= 2 { 1 } else { 0 };
        let out = if self.step == 1. { (a.frame(0, p as i64), a.frame(right, p as i64)) } else { (a.sample(0, p), a.sample(right, p)) };
        self.pos += self.step;
        self.left -= 1;
        Some((out.0 * gain, out.1 * gain))
    }
}

type Cmd = Option<Box<Voice>>;

enum Req {
    Play { path: PathBuf, ratio: f64, reverse: bool },
    /// A sound stretched to the song and transposed, entering on a bar.
    Synced(Box<SyncedPreview>, Instant),
    Stop,
    /// Play on this output (host name, device id); `None` = system default.
    Device(Option<(String, String)>),
}

pub struct Preview {
    tx: Option<Sender<Req>>,
    errors: Receiver<String>,
    err_tx: Sender<String>,
    volume: Arc<AtomicU32>,
}

impl Preview {
    pub fn new() -> Self {
        let (err_tx, errors) = crossbeam_channel::unbounded();
        Self { tx: None, errors, err_tx, volume: Arc::new(AtomicU32::new(1f32.to_bits())) }
    }

    fn worker(&mut self) -> Result<&Sender<Req>, String> {
        if self.tx.is_none() {
            let (tx, rx) = crossbeam_channel::unbounded();
            let (errors, volume) = (self.err_tx.clone(), self.volume.clone());
            std::thread::Builder::new().name("aitrack-preview".into()).spawn(move || Worker::new(errors, volume).run(rx)).map_err(|e| e.to_string())?;
            self.tx = Some(tx);
        }
        Ok(self.tx.as_ref().expect("spawned"))
    }

    fn send(&mut self, req: Req) -> Result<(), String> {
        let tx = self.worker()?;
        if tx.send(req).is_err() {
            self.tx = None;
            return Err("preview output stopped".into());
        }
        Ok(())
    }

    /// Kept for the engine bridge: the worker frees retired voices itself.
    pub fn collect(&mut self) {}

    /// Problems the worker ran into since the last call (device, decoding).
    pub fn take_errors(&mut self) -> Vec<String> {
        self.errors.try_iter().collect()
    }

    /// Follow the engine's output device (`None` = the system default).
    pub fn set_device(&mut self, device: Option<(String, String)>) {
        let _ = self.send(Req::Device(device));
    }

    /// Play `path` (decoded off this thread), or stop with `None`.
    pub fn play(&mut self, path: Option<&Path>) -> Result<(), String> {
        self.play_pitched(path, 1.0, false)
    }

    /// The level of every preview, 0..=1 (moves a voice already playing too).
    pub fn set_volume(&mut self, volume: f32) {
        self.volume.store(volume.clamp(0., 1.).to_bits(), Ordering::Relaxed);
    }

    /// Play a sound stretched to the song's tempo and transposed to its key
    /// (rendered on the worker, never here), entering on the next bar of the
    /// running song when `req.start_in` says so.
    pub fn play_synced(&mut self, req: &SyncedPreview) -> Result<(), String> {
        self.send(Req::Synced(Box::new(req.clone()), Instant::now())).map_err(|e| format!("preview output: {e}"))
    }

    /// Play `path` as a clip with these sample settings sounds: `ratio`
    /// times faster (Re-Pitch transpose), backwards when `reverse`.
    pub fn play_pitched(&mut self, path: Option<&Path>, ratio: f64, reverse: bool) -> Result<(), String> {
        let req = match path {
            Some(p) => Req::Play { path: p.to_path_buf(), ratio, reverse },
            None => Req::Stop,
        };
        self.send(req).map_err(|e| format!("preview output: {e}"))
    }
}

struct Stream {
    _stream: cpal::Stream,
    rate: u32,
    tx: rtrb::Producer<Cmd>,
    retired: rtrb::Consumer<Box<Voice>>,
    /// The device went away or the stream broke: reopen before the next play.
    lost: Arc<AtomicBool>,
}

struct Worker {
    stream: Option<Stream>,
    device: Option<(String, String)>,
    errors: Sender<String>,
    /// The last open failed and was reported; stay quiet until one works.
    reported: bool,
    volume: Arc<AtomicU32>,
    renders: Renders,
}

impl Worker {
    fn new(errors: Sender<String>, volume: Arc<AtomicU32>) -> Self {
        Self { stream: None, device: None, errors, reported: false, volume, renders: Renders::default() }
    }

    fn run(mut self, rx: Receiver<Req>) {
        loop {
            let first = match rx.recv_timeout(Duration::from_millis(250)) {
                Ok(r) => r,
                Err(RecvTimeoutError::Timeout) => {
                    self.collect();
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => break,
            };
            // Only the newest play request matters (fast clicking through
            // the browser must not queue up decodes); device changes all apply.
            let mut play = None;
            for req in std::iter::once(first).chain(rx.try_iter()) {
                match req {
                    Req::Device(d) => {
                        if d != self.device {
                            self.device = d;
                            self.stream = None;
                        }
                    }
                    other => play = Some(other),
                }
            }
            match play {
                Some(Req::Play { path, ratio, reverse }) => self.play(&path, ratio, reverse),
                Some(Req::Synced(req, at)) => self.play_synced(&req, at),
                Some(Req::Stop) => self.push(None),
                _ => {}
            }
            self.collect();
        }
    }

    fn collect(&mut self) {
        if let Some(s) = &mut self.stream {
            while s.retired.pop().is_ok() {}
        }
    }

    fn push(&mut self, cmd: Cmd) {
        if let Some(s) = &mut self.stream {
            // A full queue (callback not running) drops the request.
            let _ = s.tx.push(cmd);
        }
    }

    fn ensure_stream(&mut self) -> Option<u32> {
        if self.stream.as_ref().is_some_and(|s| s.lost.load(Ordering::Acquire)) {
            self.stream = None;
        }
        if self.stream.is_none() {
            match open(self.device.as_ref()) {
                Ok(s) => {
                    self.stream = Some(s);
                    self.reported = false;
                }
                Err(e) => {
                    if !self.reported {
                        let _ = self.errors.send(format!("Preview output: {e}"));
                        self.reported = true;
                    }
                    return None;
                }
            }
        }
        self.stream.as_ref().map(|s| s.rate)
    }

    fn play(&mut self, path: &Path, ratio: f64, reverse: bool) {
        // Open first (cheap once open) so decoding knows nothing about it.
        let Some(rate) = self.ensure_stream() else { return };
        match load_for_preview(path, rate) {
            Ok(audio) => {
                let mut voice = Voice::new(audio, rate, ratio, reverse);
                voice.volume = self.volume.clone();
                self.push(Some(Box::new(voice)));
            }
            Err(e) => {
                let _ = self.errors.send(format!("Preview {}: {e}", path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())));
            }
        }
    }
}

impl Worker {
    fn play_synced(&mut self, req: &SyncedPreview, asked_at: Instant) {
        let Some(rate) = self.ensure_stream() else { return };
        match self.renders.get(req, rate) {
            Ok(audio) => {
                // The render took a while: the entry is the next bar line from now, not the one that passed.
                let wait = entry_delay(req.start_in, req.bar, asked_at.elapsed());
                let delay = (wait.as_secs_f64() * f64::from(rate)).round() as usize;
                self.push(Some(Box::new(Voice::synced(audio, rate, req.looped, delay, self.volume.clone()))));
            }
            Err(e) => {
                let _ = self.errors.send(format!("Preview {}: {e}", req.path.file_name().map_or_else(|| req.path.display().to_string(), |n| n.to_string_lossy().into_owned())));
            }
        }
    }
}

/// How long after now a synced preview should start. `start_in` is the time
/// to the next bar line as of the request, `elapsed` how long ago that was
/// (the render takes time). A bar line that has gone by is replaced by the
/// following one, so the entry stays on the song's grid.
pub fn entry_delay(start_in: Option<Duration>, bar: Duration, elapsed: Duration) -> Duration {
    let Some(start_in) = start_in else { return Duration::ZERO };
    if elapsed <= start_in {
        return start_in - elapsed;
    }
    let bar = bar.as_secs_f64();
    if bar <= 0. {
        return Duration::ZERO;
    }
    let late = (elapsed - start_in).as_secs_f64();
    let into_bar = late % bar;
    Duration::from_secs_f64(if into_bar < 1e-9 { 0. } else { bar - into_bar })
}

/// Renders kept for going back and forth in a list: the same sound at the
/// same tempo, key and rate is not stretched twice.
struct Renders {
    kept: VecDeque<(RenderKey, Arc<DecodedAudio>)>,
    /// Most bytes of audio the kept renders may hold together (the newest always stays).
    budget: usize,
}

impl Default for Renders {
    fn default() -> Self {
        Self { kept: VecDeque::new(), budget: RENDER_BUDGET_BYTES }
    }
}

#[derive(Clone, PartialEq)]
struct RenderKey {
    path: PathBuf,
    /// `original_bpm`, `project_bpm` as bits (`0` = not stretched).
    tempo: (u64, u64),
    semitones: i8,
    tonal: bool,
    rate: u32,
}

impl Renders {
    fn get(&mut self, req: &SyncedPreview, rate: u32) -> Result<Arc<DecodedAudio>, String> {
        let key = RenderKey {
            path: req.path.clone(),
            tempo: req.original_bpm.map_or((0, 0), |b| (b.to_bits(), req.project_bpm.to_bits())),
            semitones: req.semitones,
            tonal: req.tonal,
            rate,
        };
        if let Some(i) = self.kept.iter().position(|(k, _)| *k == key) {
            let hit = self.kept.remove(i).expect("found");
            self.kept.push_back(hit.clone());
            return Ok(hit.1);
        }
        let audio = Arc::new(render_synced(req, rate)?);
        self.kept.push_back((key, audio.clone()));
        while self.kept.len() > RENDER_KEEP || (self.kept.len() > 1 && self.bytes() > self.budget) {
            self.kept.pop_front();
        }
        Ok(audio)
    }

    fn bytes(&self) -> usize {
        self.kept.iter().map(|(_, a)| a.byte_size()).sum()
    }
}

/// The sound of `req` as it plays: decoded, stretched from its tempo to the
/// song's and transposed (pitch-preserving for tonal material, transient-aware
/// for percussion, a plain Re-Pitch for a one-shot that only changes key), then
/// converted to the stream's rate with the clips' own converter. Slow: on the worker only.
pub fn render_synced(req: &SyncedPreview, stream_rate: u32) -> Result<DecodedAudio, String> {
    let (mut channels, file_rate) = sample_cache::decode_file(&req.path)?;
    // Only the first `MAX_SECONDS` are ever heard: do not stretch or convert the rest.
    let keep = file_rate as usize * MAX_SECONDS;
    for c in &mut channels {
        c.truncate(keep);
        c.shrink_to_fit();
    }
    let time_scale = match req.original_bpm {
        Some(b) if b > 0. && req.project_bpm > 0. => b / req.project_bpm,
        _ => 1.,
    };
    let pitch = crate::bridge::semitone_ratio(req.semitones);
    let channels = if (time_scale - 1.).abs() < 1e-4 && req.semitones == 0 {
        channels
    } else {
        let algo = match (req.original_bpm.is_some(), req.tonal) {
            (true, true) => WarpAlgorithm::Complex,
            (true, false) => WarpAlgorithm::Beats,
            // No tempo: the sound is only moved in pitch, as an unwarped clip is.
            (false, _) => WarpAlgorithm::Repitch,
        };
        let refs: Vec<&[f32]> = channels.iter().map(Vec::as_slice).collect();
        warp_audio(&refs, file_rate, algo, time_scale, pitch, None)
    };
    let channels = if file_rate == stream_rate { channels } else { sample_cache::resample(&channels, file_rate, stream_rate)? };
    let mut audio = DecodedAudio::from_planar(channels, stream_rate);
    audio.source_rate = file_rate;
    Ok(audio)
}

/// Decode `path` for the preview voice. Files up to `MAX_SECONDS` go through
/// the same decode + `sample_cache::resample` pair as `sample_cache::load_audio`
/// (what a clip uses), so the browser sounds like the imported sample, with
/// no aliasing at 96 kHz and no dulled top end at 44.1 kHz. Longer files are
/// only heard for their first `MAX_SECONDS`, so they skip the whole-file
/// resample and are read at their own rate by the voice.
fn load_for_preview(path: &Path, stream_rate: u32) -> Result<DecodedAudio, String> {
    let (channels, file_rate) = sample_cache::decode_file(path)?;
    let frames = channels.iter().map(Vec::len).min().unwrap_or(0);
    if file_rate == stream_rate || frames > file_rate as usize * MAX_SECONDS {
        return Ok(DecodedAudio::from_planar(channels, file_rate));
    }
    let channels = sample_cache::resample(&channels, file_rate, stream_rate)?;
    let mut audio = DecodedAudio::from_planar(channels, stream_rate);
    audio.source_rate = file_rate;
    Ok(audio)
}

fn find_output(device: Option<&(String, String)>) -> Result<cpal::Device, String> {
    if let Some((host, id)) = device {
        let found = cpal::available_hosts()
            .into_iter()
            .find(|h| h.name().eq_ignore_ascii_case(host))
            .and_then(|h| cpal::host_from_id(h).ok())
            .and_then(|h| id.parse::<cpal::DeviceId>().ok().and_then(|i| h.device_by_id(&i)));
        if let Some(d) = found {
            return Ok(d);
        }
    }
    cpal::default_host().default_output_device().ok_or_else(|| "no output device".into())
}

fn open(device: Option<&(String, String)>) -> Result<Stream, String> {
    let device = find_output(device)?;
    let def = device.default_output_config().map_err(|e| e.to_string())?;
    if def.sample_format() != cpal::SampleFormat::F32 {
        return Err(format!("unsupported output format {:?}", def.sample_format()));
    }
    let channels = def.channels() as usize;
    let rate = def.sample_rate();
    let buffer = match def.buffer_size() {
        cpal::SupportedBufferSize::Range { min, max } if *min <= BUFFER && BUFFER <= *max => cpal::BufferSize::Fixed(BUFFER),
        cpal::SupportedBufferSize::Range { min, max } if *min <= *max => cpal::BufferSize::Fixed(BUFFER.clamp(*min, *max)),
        _ => cpal::BufferSize::Default,
    };
    let config = cpal::StreamConfig { channels: def.channels(), sample_rate: rate, buffer_size: buffer };
    let lost = Arc::new(AtomicBool::new(false));
    let build = |config: cpal::StreamConfig| {
        let (tx, mut rx) = rtrb::RingBuffer::<Cmd>::new(8);
        let (mut ret_tx, ret_rx) = rtrb::RingBuffer::<Box<Voice>>::new(16);
        let mut current: Option<Box<Voice>> = None;
        let lost_cb = lost.clone();
        device
            .build_output_stream::<f32, _, _>(
                config,
                move |data: &mut [f32], _| {
                    while let Ok(cmd) = rx.pop() {
                        if let Some(old) = std::mem::replace(&mut current, cmd) {
                            // Freed on the worker; if the queue is full, keep it alive.
                            if let Err(rtrb::PushError::Full(v)) = ret_tx.push(old) {
                                std::mem::forget(v);
                            }
                        }
                    }
                    data.fill(0.);
                    let Some(v) = current.as_mut() else { return };
                    for frame in data.chunks_mut(channels) {
                        let Some((l, r)) = v.next() else { break };
                        frame[0] = l;
                        if channels > 1 {
                            frame[1] = r;
                        }
                    }
                },
                move |e| {
                    if matches!(e.kind(), cpal::ErrorKind::DeviceNotAvailable | cpal::ErrorKind::StreamInvalidated | cpal::ErrorKind::BackendError) {
                        lost_cb.store(true, Ordering::Release);
                    }
                },
                None,
            )
            .map(|s| (s, tx, ret_rx))
            .map_err(|e| e.to_string())
    };
    // Some drivers refuse a fixed size; take theirs rather than no preview.
    let (stream, tx, retired) = match build(config) {
        Ok(b) => b,
        Err(_) if config.buffer_size != cpal::BufferSize::Default => build(cpal::StreamConfig { buffer_size: cpal::BufferSize::Default, ..config })?,
        Err(e) => return Err(e),
    };
    stream.play().map_err(|e| e.to_string())?;
    Ok(Stream { _stream: stream, rate, tx, retired, lost })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(rate: u32) -> DecodedAudio {
        DecodedAudio::from_planar(vec![(0..1000).map(|i| i as f32).collect()], rate)
    }

    fn drain(mut v: Voice) -> Vec<(f32, f32)> {
        std::iter::from_fn(|| v.next()).collect()
    }

    #[test]
    fn pitched_previews_play_faster_and_backwards() {
        let plain = drain(Voice::new(ramp(48_000), 48_000, 1., false));
        assert_eq!((plain.len(), plain[1]), (1000, (1., 1.)));
        let octave = drain(Voice::new(ramp(48_000), 48_000, 2., false));
        assert_eq!(octave.len(), 500);
        assert!((octave[100].0 - 200.).abs() < 0.5);
        let back = drain(Voice::new(ramp(48_000), 48_000, 1., true));
        assert_eq!((back[0].0, back[1].0), (999., 998.));
        let mut capped = Voice::new(ramp(48_000), 48_000, 1., false);
        capped.left = 300;
        assert_eq!(drain(capped).len(), 300, "capped");
    }

    #[test]
    fn files_play_at_their_own_rate_without_a_resample_pass() {
        // A 24 kHz file on a 48 kHz stream reads half a file frame per output frame.
        let v = Voice::new(ramp(24_000), 48_000, 1., false);
        assert!((v.step - 0.5).abs() < 1e-12);
        let out = drain(v);
        assert_eq!(out.len(), 1999);
        assert!((out[200].0 - 100.).abs() < 0.5);
        // Bad ratios fall back to unity.
        assert_eq!(Voice::new(ramp(48_000), 48_000, f64::NAN, false).step, 1.);
    }

    fn write_wav(name: &str, rate: u32, seconds: f32, amp: f32) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aitrack-preview-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let spec = hound::WavSpec { channels: 1, sample_rate: rate, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        for i in 0..(rate as f32 * seconds) as usize {
            w.write_sample(amp * (std::f32::consts::TAU * 1000. * i as f32 / rate as f32).sin()).unwrap();
        }
        w.finalize().unwrap();
        path
    }

    fn rms_db(x: impl Iterator<Item = f32>) -> f32 {
        let (sum, n) = x.fold((0f64, 0usize), |(s, n), v| (s + (v as f64).powi(2), n + 1));
        10. * (sum / n.max(1) as f64).log10() as f32
    }

    #[test]
    fn preview_matches_the_clip_level_and_conversion() {
        // 44.1 kHz file on a 48 kHz device: the clip path resamples with
        // `load_audio`; the preview must give the same samples at unity gain.
        let path = write_wav("tone-44k.wav", 44_100, 0.5, 0.8);
        let clip = sample_cache::load_audio(&path, 48_000).unwrap();
        let heard = drain(Voice::new(load_for_preview(&path, 48_000).unwrap(), 48_000, 1., false));
        let clip_left = clip.channels[0].iter().copied();
        let diff = rms_db(heard.iter().map(|f| f.0)) - rms_db(clip_left);
        assert!(diff.abs() < 0.1, "preview is {diff} dB off the clip");
        assert_eq!(heard.len(), clip.frames);
        assert!(heard.iter().zip(&clip.channels[0]).all(|(h, c)| (h.0 - c).abs() < 1e-6), "same samples as the clip");
        // And the absolute level is the file's: 0.8 sine = -3.98 dBFS RMS.
        assert!((rms_db(heard.iter().map(|f| f.0)) - (0.8f32 / 2f32.sqrt()).log10() * 20.).abs() < 0.1);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn long_previews_skip_the_resample_pass() {
        // Past 30 s only the first 30 s are heard, read at the file's own rate.
        let path = write_wav("long-8k.wav", 8_000, 31., 0.5);
        let audio = load_for_preview(&path, 48_000).unwrap();
        assert_eq!(audio.sample_rate, 8_000);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn empty_audio_is_silent() {
        let mut v = Voice::new(DecodedAudio::from_planar(Vec::new(), 48_000), 48_000, 1., false);
        assert!(v.next().is_none());
    }

    fn tone(name: &str, rate: u32, seconds: f32, hz: f32) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aitrack-preview-sync-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        let spec = hound::WavSpec { channels: 1, sample_rate: rate, bits_per_sample: 32, sample_format: hound::SampleFormat::Float };
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        for i in 0..(rate as f32 * seconds) as usize {
            w.write_sample(0.5 * (std::f32::consts::TAU * hz * i as f32 / rate as f32).sin()).unwrap();
        }
        w.finalize().unwrap();
        path
    }

    /// Pitch by counting rising zero crossings per second.
    fn hz_of(a: &DecodedAudio) -> f32 {
        let c = &a.channels[0];
        let crossings = c.windows(2).filter(|w| w[0] <= 0. && w[1] > 0.).count();
        crossings as f32 / (c.len() as f32 / a.sample_rate as f32)
    }

    fn request(path: PathBuf) -> SyncedPreview {
        SyncedPreview { path, original_bpm: None, project_bpm: 120., semitones: 0, tonal: true, looped: true, start_in: None, bar: Duration::from_secs(2) }
    }

    #[test]
    fn a_loop_is_stretched_to_the_songs_tempo_and_keeps_its_pitch() {
        // 2 s at 100 BPM in a 125 BPM song: 1.6 s, still 440 Hz.
        let path = tone("loop-100.wav", 48_000, 2., 440.);
        let req = SyncedPreview { original_bpm: Some(100.), project_bpm: 125., ..request(path.clone()) };
        let a = render_synced(&req, 48_000).unwrap();
        assert!((a.frames as f64 - 1.6 * 48_000.).abs() < 300., "{} frames", a.frames);
        assert!((hz_of(&a) - 440.).abs() < 12., "{} Hz", hz_of(&a));
        // Already at the song's tempo and key: the file as it is.
        let same = render_synced(&SyncedPreview { original_bpm: Some(120.), ..request(path.clone()) }, 48_000).unwrap();
        assert_eq!(same.frames, 96_000);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_tonal_loop_moves_up_a_fifth_without_changing_its_length() {
        let path = tone("loop-fifth.wav", 48_000, 2., 220.);
        let up = render_synced(&SyncedPreview { original_bpm: Some(120.), semitones: 7, ..request(path.clone()) }, 48_000).unwrap();
        assert!((up.frames as f64 - 96_000.).abs() < 400., "{} frames: the length is the song's", up.frames);
        let want = 220. * 2f32.powf(7. / 12.);
        assert!((hz_of(&up) - want).abs() < 15., "{} Hz, want {want}", hz_of(&up));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_one_shot_that_only_changes_key_is_repitched_like_an_unwarped_clip() {
        let path = tone("shot.wav", 48_000, 1., 300.);
        let up = render_synced(&SyncedPreview { looped: false, semitones: 12, ..request(path.clone()) }, 48_000).unwrap();
        assert!((up.frames as i64 - 24_000).abs() < 50, "an octave up plays in half the time: {}", up.frames);
        assert!((hz_of(&up) - 600.).abs() < 8.);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn the_render_is_converted_to_the_streams_rate_and_kept_for_the_next_time() {
        let path = tone("rate-44k.wav", 44_100, 1., 500.);
        let req = SyncedPreview { original_bpm: Some(120.), semitones: 2, ..request(path.clone()) };
        let mut renders = Renders::default();
        let a = renders.get(&req, 48_000).unwrap();
        assert_eq!(a.sample_rate, 48_000);
        let again = renders.get(&req, 48_000).unwrap();
        assert!(Arc::ptr_eq(&a, &again), "the same tempo, key and rate is not rendered twice");
        let other = renders.get(&SyncedPreview { semitones: 3, ..req.clone() }, 48_000).unwrap();
        assert!(!Arc::ptr_eq(&a, &other));
        for i in 0..(RENDER_KEEP + 2) {
            renders.get(&SyncedPreview { semitones: -(i as i8) - 1, ..req.clone() }, 48_000).unwrap();
        }
        assert!(renders.kept.len() <= RENDER_KEEP, "bounded");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_long_file_is_cut_to_the_preview_length_before_it_is_stretched() {
        // 90 s at 8 kHz: only the first MAX_SECONDS can be heard, so only those are rendered.
        let path = tone("long-90s.wav", 8_000, 90., 300.);
        let cap = MAX_SECONDS * 48_000;
        let plain = render_synced(&request(path.clone()), 48_000).unwrap();
        assert!(plain.frames <= cap + 48, "{} frames", plain.frames);
        let stretched = render_synced(&SyncedPreview { original_bpm: Some(120.), semitones: 3, ..request(path.clone()) }, 48_000).unwrap();
        assert!(stretched.frames <= cap + 4_800, "{} frames", stretched.frames);
        assert!(stretched.byte_size() < 32 << 20, "{} bytes", stretched.byte_size());
        // A short file is untouched.
        let short = tone("short-2s.wav", 8_000, 2., 300.);
        assert_eq!(render_synced(&request(short.clone()), 8_000).unwrap().frames, 16_000);
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(short);
    }

    #[test]
    fn the_renders_kept_are_bounded_by_size_not_only_by_count() {
        let path = tone("budget-8k.wav", 8_000, 4., 400.);
        let req = SyncedPreview { original_bpm: Some(120.), ..request(path.clone()) };
        let one = render_synced(&req, 48_000).unwrap().byte_size();
        // Room for two and a half renders: never more than two are held, the newest always is.
        let mut renders = Renders { budget: one * 5 / 2, ..Renders::default() };
        let mut last = None;
        for i in 0..6 {
            last = Some(renders.get(&SyncedPreview { semitones: i, ..req.clone() }, 48_000).unwrap());
            assert!(renders.kept.len() <= 2, "{} kept", renders.kept.len());
            assert!(renders.bytes() <= renders.budget);
        }
        let again = renders.get(&SyncedPreview { semitones: 5, ..req.clone() }, 48_000).unwrap();
        assert!(Arc::ptr_eq(&last.unwrap(), &again), "the newest survives");
        // One bigger than the whole budget is still served (and kept alone).
        let mut tiny = Renders { budget: 1, ..Renders::default() };
        let a = tiny.get(&req, 48_000).unwrap();
        assert!(a.frames > 0 && tiny.kept.len() <= 1);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_loop_enters_on_the_next_bar_of_the_running_song_even_when_the_render_is_slow() {
        let (bar, ms) = (Duration::from_millis(2000), Duration::from_millis);
        // Stopped: at once.
        assert_eq!(entry_delay(None, bar, ms(300)), Duration::ZERO);
        // 1.5 s to the bar line, the render took 0.3 s: 1.2 s left.
        assert_eq!(entry_delay(Some(ms(1500)), bar, ms(300)), ms(1200));
        // The render took longer than the wait: the line after that one (2 s later).
        let late = entry_delay(Some(ms(500)), bar, ms(900));
        assert_eq!(late, ms(1600), "bar lines at 0.5 s and 2.5 s: now is 0.9 s");
        // Exactly on a line: no wait.
        assert_eq!(entry_delay(Some(ms(500)), bar, ms(2500)), Duration::ZERO);
        // Always inside one bar once it is late.
        for e in 0..20_000u64 {
            assert!(entry_delay(Some(ms(700)), bar, ms(e)) <= bar.max(ms(700)));
        }
    }

    #[test]
    fn a_synced_voice_waits_its_delay_loops_and_follows_the_volume() {
        let vol = Arc::new(AtomicU32::new(0.5f32.to_bits()));
        let audio = Arc::new(DecodedAudio::from_planar(vec![vec![1., 2., 3.]], 48_000));
        let mut v = Voice::synced(audio, 48_000, true, 2, vol.clone());
        let first: Vec<f32> = (0..8).map(|_| v.next().unwrap().0).collect();
        assert_eq!(first, [0., 0., 0.5, 1., 1.5, 0.5, 1., 1.5], "two frames of delay, then the loop twice at half level");
        vol.store(1f32.to_bits(), Ordering::Relaxed);
        assert_eq!(v.next().unwrap().0, 1., "the loop starts a third round, now at full level: the volume moves a voice that is playing");
        let once = Voice::synced(Arc::new(DecodedAudio::from_planar(vec![vec![1., 2.]], 48_000)), 48_000, false, 0, vol);
        assert_eq!(drain(once).len(), 2, "a one-shot plays once");
    }
}
