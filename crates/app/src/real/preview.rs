//! A small preview voice for the browser: plays one file next to the
//! engine's stream, on the same output device.
//!
//! Everything slow happens on one worker thread (`aitrack-preview`): opening
//! the device, decoding the file and freeing finished voices. The UI thread
//! only sends a request. The file is decoded at its own rate and read by the
//! callback through the engine's interpolator at `ratio × file rate / stream
//! rate`, so playback starts as soon as the file is decoded (no whole-file
//! resample or pre-render) and a 256-frame buffer keeps the click-to-sound
//! time short. The callback swaps voices from a wait-free queue and hands
//! the old one back on another, so it never allocates or frees.

use aitrack_engine::sample_cache::{self, DecodedAudio};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// −6 dB: close to how the sample will sound in a track, with headroom for
/// files normalised to full scale.
const GAIN: f32 = 0.5;
/// Longest preview.
const MAX_SECONDS: usize = 30;
/// Frames per callback asked of the device (clamped to what it supports).
const BUFFER: u32 = 256;

/// One file playing: read position in file frames, advanced by `step`.
struct Voice {
    audio: DecodedAudio,
    pos: f64,
    step: f64,
    reverse: bool,
    /// Output frames left before the preview stops.
    left: usize,
}

impl Voice {
    fn new(audio: DecodedAudio, stream_rate: u32, ratio: f64, reverse: bool) -> Self {
        let ratio = if ratio.is_finite() && ratio > 0. { ratio } else { 1. };
        let step = ratio * audio.sample_rate as f64 / stream_rate.max(1) as f64;
        Self { audio, pos: 0., step, reverse, left: stream_rate as usize * MAX_SECONDS }
    }

    /// The next stereo frame, or `None` at the end. Real-time safe.
    #[inline]
    fn next(&mut self) -> Option<(f32, f32)> {
        let a = &self.audio;
        let last = a.frames as f64 - 1.;
        if self.left == 0 || a.channels.is_empty() || self.pos > last {
            return None;
        }
        let p = if self.reverse { last - self.pos } else { self.pos };
        let right = if a.channels.len() >= 2 { 1 } else { 0 };
        let out = if self.step == 1. { (a.frame(0, p as i64), a.frame(right, p as i64)) } else { (a.sample(0, p), a.sample(right, p)) };
        self.pos += self.step;
        self.left -= 1;
        Some(out)
    }
}

type Cmd = Option<Box<Voice>>;

enum Req {
    Play { path: PathBuf, ratio: f64, reverse: bool },
    Stop,
    /// Play on this output (host name, device id); `None` = system default.
    Device(Option<(String, String)>),
}

pub struct Preview {
    tx: Option<Sender<Req>>,
    errors: Receiver<String>,
    err_tx: Sender<String>,
}

impl Preview {
    pub fn new() -> Self {
        let (err_tx, errors) = crossbeam_channel::unbounded();
        Self { tx: None, errors, err_tx }
    }

    fn worker(&mut self) -> Result<&Sender<Req>, String> {
        if self.tx.is_none() {
            let (tx, rx) = crossbeam_channel::unbounded();
            let errors = self.err_tx.clone();
            std::thread::Builder::new().name("aitrack-preview".into()).spawn(move || Worker::new(errors).run(rx)).map_err(|e| e.to_string())?;
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
}

impl Worker {
    fn new(errors: Sender<String>) -> Self {
        Self { stream: None, device: None, errors, reported: false }
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
        match sample_cache::decode_file(path) {
            Ok((channels, file_rate)) => {
                let voice = Voice::new(DecodedAudio::from_planar(channels, file_rate), rate, ratio, reverse);
                self.push(Some(Box::new(voice)));
            }
            Err(e) => {
                let _ = self.errors.send(format!("Preview {}: {e}", path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())));
            }
        }
    }
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
                        frame[0] = l * GAIN;
                        if channels > 1 {
                            frame[1] = r * GAIN;
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

    #[test]
    fn empty_audio_is_silent() {
        let mut v = Voice::new(DecodedAudio::from_planar(Vec::new(), 48_000), 48_000, 1., false);
        assert!(v.next().is_none());
    }
}
