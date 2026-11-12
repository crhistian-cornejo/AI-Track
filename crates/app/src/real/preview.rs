//! A small preview voice for the browser: plays one file on the default
//! output, in its own stream next to the engine's.
//!
//! Files are decoded and resampled on a worker thread. The callback swaps
//! in the new voice from a wait-free queue and hands the old one back on
//! another, so it never allocates or frees.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;
use std::path::Path;
use std::sync::Arc;

/// −6 dB: close to how the sample will sound in a track, with headroom for
/// files normalised to full scale.
const GAIN: f32 = 0.5;
/// Longest preview.
const MAX_SECONDS: usize = 30;

struct Voice {
    /// Interleaved stereo at the stream rate.
    frames: Vec<f32>,
    pos: usize,
}

type Cmd = Option<Box<Voice>>;

pub struct Preview {
    stream: Option<cpal::Stream>,
    rate: u32,
    tx: Option<Arc<Mutex<rtrb::Producer<Cmd>>>>,
    retired: Option<rtrb::Consumer<Box<Voice>>>,
    failed: bool,
}

impl Preview {
    pub fn new() -> Self {
        Self { stream: None, rate: 48_000, tx: None, retired: None, failed: false }
    }

    fn open(&mut self) -> Result<(), String> {
        if self.stream.is_some() {
            return Ok(());
        }
        let device = cpal::default_host().default_output_device().ok_or("no output device")?;
        let def = device.default_output_config().map_err(|e| e.to_string())?;
        if def.sample_format() != cpal::SampleFormat::F32 {
            return Err(format!("unsupported output format {:?}", def.sample_format()));
        }
        let channels = def.channels() as usize;
        let rate = def.sample_rate();
        let config = cpal::StreamConfig { channels: def.channels(), sample_rate: rate, buffer_size: cpal::BufferSize::Default };
        let (tx, mut rx) = rtrb::RingBuffer::<Cmd>::new(8);
        let (mut ret_tx, ret_rx) = rtrb::RingBuffer::<Box<Voice>>::new(16);
        let mut current: Option<Box<Voice>> = None;
        let stream = device
            .build_output_stream::<f32, _, _>(
                config,
                move |data: &mut [f32], _| {
                    while let Ok(cmd) = rx.pop() {
                        if let Some(old) = std::mem::replace(&mut current, cmd) {
                            // Freed on the UI thread; if the queue is full, keep it alive.
                            if let Err(rtrb::PushError::Full(v)) = ret_tx.push(old) {
                                std::mem::forget(v);
                            }
                        }
                    }
                    data.fill(0.);
                    let Some(v) = current.as_mut() else { return };
                    for frame in data.chunks_mut(channels) {
                        if v.pos + 1 >= v.frames.len() {
                            break;
                        }
                        let (l, r) = (v.frames[v.pos] * GAIN, v.frames[v.pos + 1] * GAIN);
                        v.pos += 2;
                        frame[0] = l;
                        if channels > 1 {
                            frame[1] = r;
                        }
                    }
                },
                |e| log::warn!("preview stream: {e}"),
                None,
            )
            .map_err(|e| e.to_string())?;
        stream.play().map_err(|e| e.to_string())?;
        self.stream = Some(stream);
        self.rate = rate;
        self.tx = Some(Arc::new(Mutex::new(tx)));
        self.retired = Some(ret_rx);
        Ok(())
    }

    /// Free voices the callback handed back.
    pub fn collect(&mut self) {
        if let Some(r) = &mut self.retired {
            while r.pop().is_ok() {}
        }
    }

    /// Play `path` (decoded off this thread), or stop with `None`.
    pub fn play(&mut self, path: Option<&Path>) -> Result<(), String> {
        self.play_pitched(path, 1.0, false)
    }

    /// Play `path` as a clip with these sample settings sounds: `ratio`
    /// times faster (Re-Pitch transpose), backwards when `reverse`.
    pub fn play_pitched(&mut self, path: Option<&Path>, ratio: f64, reverse: bool) -> Result<(), String> {
        self.collect();
        if self.failed {
            return Err("preview output unavailable".into());
        }
        if let Err(e) = self.open() {
            self.failed = true;
            return Err(format!("preview output: {e}"));
        }
        let tx = self.tx.clone().expect("opened");
        let Some(path) = path else {
            let _ = tx.lock().push(None);
            return Ok(());
        };
        let path = path.to_path_buf();
        let rate = self.rate;
        std::thread::Builder::new()
            .name("aitrack-preview-decode".into())
            .spawn(move || match aitrack_engine::sample_cache::load_audio(&path, rate) {
                Ok(a) => {
                    let frames = render(&a, rate as usize * MAX_SECONDS, ratio, reverse);
                    let _ = tx.lock().push(Some(Box::new(Voice { frames, pos: 0 })));
                }
                Err(e) => log::warn!("preview {}: {e}", path.display()),
            })
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

/// Interleaved stereo of `a` as it plays `ratio` times faster (read through
/// the engine's interpolator) and backwards when `reverse`, at most `max`
/// frames long.
fn render(a: &aitrack_engine::sample_cache::DecodedAudio, max: usize, ratio: f64, reverse: bool) -> Vec<f32> {
    if a.channels.is_empty() {
        return Vec::new();
    }
    let ratio = if ratio.is_finite() && ratio > 0. { ratio } else { 1. };
    let n = ((a.frames as f64 / ratio).floor() as usize).min(max);
    let right = if a.channels.len() >= 2 { 1 } else { 0 };
    let last = a.frames as f64 - 1.;
    let mut out = Vec::with_capacity(n * 2);
    for i in 0..n {
        let pos = i as f64 * ratio;
        let pos = if reverse { last - pos } else { pos };
        if ratio == 1. {
            out.push(a.frame(0, pos as i64));
            out.push(a.frame(right, pos as i64));
        } else {
            out.push(a.sample(0, pos));
            out.push(a.sample(right, pos));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pitched_previews_play_faster_and_backwards() {
        let ramp: Vec<f32> = (0..1000).map(|i| i as f32).collect();
        let a = aitrack_engine::sample_cache::DecodedAudio::from_planar(vec![ramp], 48_000);
        let plain = render(&a, 10_000, 1., false);
        assert_eq!((plain.len(), plain[2], plain[3]), (2000, 1., 1.));
        let octave = render(&a, 10_000, 2., false);
        assert_eq!(octave.len(), 1000);
        assert!((octave[2 * 100] - 200.).abs() < 0.5);
        let back = render(&a, 10_000, 1., true);
        assert_eq!((back[0], back[2]), (999., 998.));
        assert_eq!(render(&a, 300, 1., false).len(), 600, "capped");
    }
}
