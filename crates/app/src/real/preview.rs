//! A small preview voice for the browser: plays one file at a low level on
//! the default output, in its own stream next to the engine's.
//!
//! Files are decoded and resampled on a worker thread. The callback swaps
//! in the new voice from a wait-free queue and hands the old one back on
//! another, so it never allocates or frees.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use parking_lot::Mutex;
use std::path::Path;
use std::sync::Arc;

/// About −18 dB: previews sit well under the mix.
const GAIN: f32 = 0.125;
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
                    let n = a.frames.min(rate as usize * MAX_SECONDS);
                    let left = a.channels.first();
                    let right = a.channels.get(1).or(left);
                    let mut frames = Vec::with_capacity(n * 2);
                    if let (Some(l), Some(r)) = (left, right) {
                        for i in 0..n {
                            frames.push(l[i]);
                            frames.push(r[i]);
                        }
                    }
                    let _ = tx.lock().push(Some(Box::new(Voice { frames, pos: 0 })));
                }
                Err(e) => log::warn!("preview {}: {e}", path.display()),
            })
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}
