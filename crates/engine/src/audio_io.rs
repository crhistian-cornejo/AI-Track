//! Audio device I/O through cpal 0.18.
//!
//! Hosts: WASAPI on Windows (shared mode; exclusive mode via the `wasapi`
//! crate is a later step), CoreAudio on macOS, and ASIO behind the
//! disabled-by-default `asio` cargo feature. ASIO must be built against the
//! Steinberg SDK under its free **proprietary** licence (signed agreement),
//! never its GPLv3 option, which would force the DAW to GPL.
//!
//! The output callback promotes its thread once (MMCSS "Pro Audio"), flushes
//! denormals and runs the [`AudioProcessor`]. Stream errors that mean the
//! device is gone set a flag the control thread polls to reopen a device;
//! a reroute of the system default output (headphones plugged in, Bluetooth
//! connected) sets another so the stream is rebuilt at the new device's own
//! rate and channel count instead of running through the OS's converter.
//!
//! Latency: every callback measures `playback − callback` (output) and
//! `callback − capture` (input) from cpal's stream timestamps, which on
//! CoreAudio already include the device latency and safety offset and on
//! Bluetooth the link delay, and publishes them in [`SharedStatus::io`].

use crate::rt::AudioProcessor;
use crate::status::SharedStatus;
use crate::EngineError;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample};
use rtrb::Producer;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::Arc;

/// A device as listed for the settings UI.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioDeviceInfo {
    /// Host name ("WASAPI", "ASIO", "CoreAudio").
    pub host: String,
    /// Stable id (`host:device`) to pass back in [`DeviceConfig`].
    pub id: String,
    pub name: String,
    pub is_default_output: bool,
    pub is_default_input: bool,
    pub output_channels: u16,
    pub input_channels: u16,
    pub default_sample_rate: Option<u32>,
    /// Common rates the device accepts.
    pub sample_rates: Vec<u32>,
    /// Supported buffer sizes in frames, when the host knows them.
    pub buffer_range: Option<(u32, u32)>,
}

/// WASAPI sharing mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShareMode {
    #[default]
    Shared,
    /// WASAPI exclusive mode. cpal 0.18 only opens shared streams, so this
    /// currently falls back to shared (reported in [`ActiveAudio::notes`]);
    /// a dedicated backend on the `wasapi` crate is the planned fix.
    Exclusive,
}

/// A contiguous range of device channels: `first` (0-based) and `count`
/// (1 = mono, 2 = stereo pair).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChannelSelection {
    pub first: u16,
    pub count: u16,
}

impl Default for ChannelSelection {
    fn default() -> Self {
        Self { first: 0, count: 2 }
    }
}

/// Persistent audio settings (serialised in the app's preferences). Every
/// field is optional so a stale file still opens *something*: missing
/// devices fall back to the default device, unsupported rates to the
/// device's default rate, buffer sizes are clamped to what the device and
/// the engine (32–4096) support.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AudioConfig {
    /// Host/driver name ("WASAPI", "ASIO", "CoreAudio"); `None` = default.
    pub host: Option<String>,
    pub share_mode: ShareMode,
    /// Output device id from [`AudioDeviceInfo::id`]; `None` = default.
    pub output_device: Option<String>,
    /// Display name, used to find the device again if its id changed.
    pub output_device_name: Option<String>,
    /// Device channels that receive the engine's stereo main output.
    pub output_channels: ChannelSelection,
    /// Open an input stream for monitoring/recording.
    pub input_enabled: bool,
    pub input_device: Option<String>,
    pub input_device_name: Option<String>,
    /// Device channels that feed the engine's stereo input.
    pub input_channels: ChannelSelection,
    /// 44100, 48000, 88200, 96000, 176400 or 192000 (whatever the device supports).
    pub sample_rate: Option<u32>,
    /// Frames per callback, 32–4096 where supported; `None` = device default.
    pub buffer_size: Option<u32>,
}

/// What was actually opened, with any fallbacks that were applied.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ActiveAudio {
    pub host: String,
    pub output_device: String,
    pub output_device_id: String,
    pub input_device: Option<String>,
    pub sample_rate: u32,
    pub buffer_size: Option<u32>,
    pub output_channels: u16,
    pub input_channels: u16,
    /// Human-readable notes about fallbacks ("device X missing, using default").
    pub notes: Vec<String>,
    /// The output is the system default device (no device was asked for):
    /// the stream is rebuilt when the system default changes.
    pub follows_default: bool,
    /// A specific output device was asked for but is missing; the engine
    /// switches back to it when it reappears.
    pub wanted_missing: bool,
}

pub const MIN_BUFFER: u32 = 32;
pub const MAX_BUFFER: u32 = 4096;

/// Buffer size used when the settings do not name one: 256 frames (5.3 ms
/// at 48 kHz) is safe on every built-in device and low enough to play.
pub const DEFAULT_BUFFER: u32 = 256;

pub const COMMON_RATES: [u32; 6] = [44_100, 48_000, 88_200, 96_000, 176_400, 192_000];

/// Names of the audio hosts available on this machine.
pub fn hosts() -> Vec<String> {
    cpal::available_hosts().into_iter().map(|h| h.name().to_string()).collect()
}

fn host_by_name(name: Option<&str>) -> Result<cpal::Host, EngineError> {
    match name {
        None => Ok(cpal::default_host()),
        Some(n) => {
            let id = cpal::available_hosts()
                .into_iter()
                .find(|h| h.name().eq_ignore_ascii_case(n))
                .ok_or_else(|| EngineError::Audio(format!("audio host '{n}' not available")))?;
            cpal::host_from_id(id).map_err(|e| EngineError::Audio(e.to_string()))
        }
    }
}

fn device_id(d: &cpal::Device) -> String {
    d.id().map(|i| i.to_string()).unwrap_or_default()
}

fn device_name(d: &cpal::Device) -> String {
    d.description().map(|desc| desc.name().to_string()).unwrap_or_else(|_| "Unknown device".into())
}

/// Every input/output device of every available host.
pub fn devices() -> Vec<AudioDeviceInfo> {
    let mut out = Vec::new();
    for host_id in cpal::available_hosts() {
        let Ok(host) = cpal::host_from_id(host_id) else { continue };
        let def_out = host.default_output_device().map(|d| device_id(&d));
        let def_in = host.default_input_device().map(|d| device_id(&d));
        let Ok(devs) = host.devices() else { continue };
        for d in devs {
            let id = device_id(&d);
            let mut info = AudioDeviceInfo {
                host: host_id.name().to_string(),
                name: device_name(&d),
                is_default_output: def_out.as_deref() == Some(id.as_str()),
                is_default_input: def_in.as_deref() == Some(id.as_str()),
                id,
                output_channels: 0,
                input_channels: 0,
                default_sample_rate: None,
                sample_rates: Vec::new(),
                buffer_range: None,
            };
            if let Ok(cfgs) = d.supported_output_configs() {
                for c in cfgs {
                    info.output_channels = info.output_channels.max(c.channels());
                    for r in COMMON_RATES {
                        if r >= c.min_sample_rate() && r <= c.max_sample_rate() && !info.sample_rates.contains(&r) {
                            info.sample_rates.push(r);
                        }
                    }
                    if let cpal::SupportedBufferSize::Range { min, max } = c.buffer_size() {
                        info.buffer_range = Some((*min, *max));
                    }
                }
            }
            if let Ok(cfgs) = d.supported_input_configs() {
                for c in cfgs {
                    info.input_channels = info.input_channels.max(c.channels());
                }
            }
            info.default_sample_rate = d.default_output_config().ok().map(|c| c.sample_rate());
            info.sample_rates.sort_unstable();
            out.push(info);
        }
    }
    out
}

/// Devices and stream configs chosen for an [`AudioConfig`], before the
/// streams are built (so the engine knows the sample rate first).
pub(crate) struct Prepared {
    output: cpal::Device,
    out_config: cpal::StreamConfig,
    out_format: SampleFormat,
    input: Option<(cpal::Device, cpal::StreamConfig, SampleFormat)>,
    pub sample_rate: u32,
    pub active: ActiveAudio,
    /// First device channel of the stereo output / input.
    pub out_first: usize,
    pub in_first: usize,
    pub in_count: usize,
}

fn find_device(host: &cpal::Host, id: Option<&str>, name: Option<&str>, output: bool) -> Option<cpal::Device> {
    if let Some(id) = id
        && let Some(d) = id.parse::<cpal::DeviceId>().ok().and_then(|p| host.device_by_id(&p)) {
            return Some(d);
        }
    let name = name?;
    let devs: Vec<cpal::Device> =
        if output { host.output_devices().ok()?.collect() } else { host.input_devices().ok()?.collect() };
    devs.into_iter().find(|d| device_name(d) == name)
}

/// Whether the output device `cfg` asks for is present (for switching back
/// after a fallback). Control thread only.
pub(crate) fn output_present(cfg: &AudioConfig) -> bool {
    if cfg.output_device.is_none() && cfg.output_device_name.is_none() {
        return false;
    }
    let Ok(host) = host_by_name(cfg.host.as_deref()) else { return false };
    find_device(&host, cfg.output_device.as_deref(), cfg.output_device_name.as_deref(), true).is_some()
}

fn supports_rate(configs: &[cpal::SupportedStreamConfigRange], rate: u32) -> bool {
    configs.iter().any(|c| rate >= c.min_sample_rate() && rate <= c.max_sample_rate())
}

/// The frames-per-callback to ask for: the requested size (or
/// [`DEFAULT_BUFFER`]) clamped to the device's range and the engine's
/// 32–4096. A range that does not intersect ours leaves the driver default.
fn pick_buffer(requested: Option<u32>, range: &cpal::SupportedBufferSize, notes: &mut Vec<String>) -> cpal::BufferSize {
    let n = requested.unwrap_or(DEFAULT_BUFFER);
    let (lo, hi) = match range {
        cpal::SupportedBufferSize::Range { min, max } => ((*min).max(MIN_BUFFER), (*max).min(MAX_BUFFER)),
        cpal::SupportedBufferSize::Unknown => (MIN_BUFFER, MAX_BUFFER),
    };
    if lo > hi {
        return cpal::BufferSize::Default;
    }
    let clamped = n.clamp(lo, hi);
    if clamped != n && requested.is_some() {
        notes.push(format!("buffer size {n} not supported, using {clamped}"));
    }
    cpal::BufferSize::Fixed(clamped)
}

fn clamp_selection(sel: ChannelSelection, max: u16, what: &str, notes: &mut Vec<String>) -> ChannelSelection {
    let mut sel = ChannelSelection { first: sel.first, count: sel.count.clamp(1, 2) };
    if sel.first + sel.count > max {
        notes.push(format!("{what} channels {}–{} not available; using 1–{}", sel.first + 1, sel.first + sel.count, 2.min(max)));
        sel = ChannelSelection { first: 0, count: 2.min(max) };
    }
    sel
}

pub(crate) fn prepare(cfg: &AudioConfig) -> Result<Prepared, EngineError> {
    let mut notes = Vec::new();
    let host = match host_by_name(cfg.host.as_deref()) {
        Ok(h) => h,
        Err(e) => {
            notes.push(format!("{e}; using the default host"));
            cpal::default_host()
        }
    };
    if cfg.share_mode == ShareMode::Exclusive {
        notes.push("WASAPI exclusive mode is not available yet; using shared mode".into());
    }
    let wanted_out = cfg.output_device.is_some() || cfg.output_device_name.is_some();
    let mut wanted_missing = false;
    let output = match find_device(&host, cfg.output_device.as_deref(), cfg.output_device_name.as_deref(), true) {
        Some(d) => d,
        None => {
            if wanted_out {
                wanted_missing = true;
                notes.push(format!(
                    "output device '{}' not found; using the default device",
                    cfg.output_device_name.as_deref().or(cfg.output_device.as_deref()).unwrap_or("?")
                ));
            }
            host.default_output_device().ok_or_else(|| EngineError::Audio("no output device available".into()))?
        }
    };
    let def = output.default_output_config().map_err(|e| EngineError::Audio(e.to_string()))?;
    let configs: Vec<_> = output.supported_output_configs().map(|c| c.collect()).unwrap_or_default();
    let sample_rate = match cfg.sample_rate {
        Some(r) if supports_rate(&configs, r) => r,
        Some(r) => {
            notes.push(format!("{r} Hz not supported by the device; using {} Hz", def.sample_rate()));
            def.sample_rate()
        }
        None => def.sample_rate(),
    };
    let max_channels = configs.iter().map(|c| c.channels()).max().unwrap_or(def.channels()).max(1);
    let sel = clamp_selection(cfg.output_channels, max_channels, "output", &mut notes);
    // Open enough channels to reach the selected pair.
    let channels = def.channels().max(sel.first + sel.count).min(max_channels);
    let buffer_size = pick_buffer(cfg.buffer_size, def.buffer_size(), &mut notes);
    let out_config = cpal::StreamConfig { channels, sample_rate, buffer_size };

    let mut in_sel = cfg.input_channels;
    let mut input = None;
    if cfg.input_enabled {
        let wanted = cfg.input_device.is_some() || cfg.input_device_name.is_some();
        let dev = find_device(&host, cfg.input_device.as_deref(), cfg.input_device_name.as_deref(), false);
        if dev.is_none() && wanted {
            notes.push("input device not found; using the default input".into());
        }
        match dev.or_else(|| host.default_input_device()) {
            Some(d) => match d.default_input_config() {
                Ok(c) => {
                    let in_configs: Vec<_> = d.supported_input_configs().map(|c| c.collect()).unwrap_or_default();
                    if supports_rate(&in_configs, sample_rate) {
                        let max_in = in_configs.iter().map(|c| c.channels()).max().unwrap_or(c.channels()).max(1);
                        in_sel = clamp_selection(in_sel, max_in, "input", &mut notes);
                        let ch = c.channels().max(in_sel.first + in_sel.count).min(max_in);
                        // Same frames per callback as the output, so input
                        // latency does not sit on a bigger driver default.
                        let wanted = match buffer_size {
                            cpal::BufferSize::Fixed(n) => Some(n),
                            cpal::BufferSize::Default => None,
                        };
                        let buffer_size = pick_buffer(wanted, c.buffer_size(), &mut Vec::new());
                        let conf = cpal::StreamConfig { channels: ch, sample_rate, buffer_size };
                        input = Some((d, conf, c.sample_format()));
                    } else {
                        notes.push(format!("input device does not run at {sample_rate} Hz; input disabled"));
                    }
                }
                Err(e) => notes.push(format!("input disabled: {e}")),
            },
            None => notes.push("no input device available".into()),
        }
    }
    let active = ActiveAudio {
        host: host.id().name().to_string(),
        output_device: device_name(&output),
        output_device_id: device_id(&output),
        input_device: input.as_ref().map(|i| device_name(&i.0)),
        sample_rate,
        buffer_size: match buffer_size {
            cpal::BufferSize::Fixed(n) => Some(n),
            cpal::BufferSize::Default => None,
        },
        output_channels: out_config.channels,
        input_channels: input.as_ref().map_or(0, |i| i.1.channels),
        notes,
        follows_default: !wanted_out || wanted_missing,
        wanted_missing,
    };
    Ok(Prepared {
        out_format: def.sample_format(),
        output,
        out_config,
        input,
        sample_rate,
        active,
        out_first: sel.first as usize,
        in_first: in_sel.first as usize,
        in_count: in_sel.count as usize,
    })
}

/// Running streams. Dropping this stops audio.
pub(crate) struct OpenStream {
    _out: cpal::Stream,
    _input: Option<cpal::Stream>,
    /// The device is gone or the stream is invalid (rate changed under it).
    pub lost: Arc<AtomicBool>,
    /// The system default output changed and the stream was rerouted.
    pub rerouted: Arc<AtomicBool>,
    pub active: ActiveAudio,
}

/// Flags the stream error callbacks raise for the control thread.
#[derive(Clone)]
struct Flags {
    status: Arc<SharedStatus>,
    lost: Arc<AtomicBool>,
    rerouted: Arc<AtomicBool>,
}

impl Flags {
    fn on_error(&self, kind: cpal::ErrorKind) {
        if kind == cpal::ErrorKind::Xrun {
            self.status.xruns.fetch_add(1, Ordering::Relaxed);
        } else if kind == cpal::ErrorKind::DeviceChanged {
            self.rerouted.store(true, Ordering::Release);
        } else if is_fatal(kind) {
            self.lost.store(true, Ordering::Release);
        }
    }
}

/// Moves a value into a stream callback only if the stream gets built: the
/// callback takes it with one atomic swap on its first call (wait-free, no
/// lock, no allocation); after a failed build the caller takes it back.
struct Handoff<T>(AtomicPtr<T>);

// SAFETY: the pointer is owned by exactly one side at a time (whoever
// swapped it out), so sharing the cell is sound when `T` can move threads.
unsafe impl<T: Send> Send for Handoff<T> {}
unsafe impl<T: Send> Sync for Handoff<T> {}

impl<T> Handoff<T> {
    fn new(value: Option<T>) -> Self {
        Self(AtomicPtr::new(value.map_or(std::ptr::null_mut(), |v| Box::into_raw(Box::new(v)))))
    }

    fn take(&self) -> Option<Box<T>> {
        let p = self.0.swap(std::ptr::null_mut(), Ordering::AcqRel);
        // SAFETY: a non-null pointer came from `Box::into_raw` and the swap
        // made this call its only owner.
        (!p.is_null()).then(|| unsafe { Box::from_raw(p) })
    }
}

impl<T> Drop for Handoff<T> {
    fn drop(&mut self) {
        drop(self.take());
    }
}

/// Frames between two stream instants, at `rate` (0 when out of order).
fn frames_between(later: cpal::StreamInstant, earlier: cpal::StreamInstant, rate: u32) -> u32 {
    let d = later.duration_since(earlier);
    (d.as_secs_f64() * rate as f64).round().min(u32::MAX as f64) as u32
}

fn is_fatal(kind: cpal::ErrorKind) -> bool {
    matches!(
        kind,
        cpal::ErrorKind::DeviceNotAvailable | cpal::ErrorKind::StreamInvalidated | cpal::ErrorKind::BackendError
    )
}

fn build_output<T>(
    p: &Prepared,
    config: cpal::StreamConfig,
    runner: &mut Option<AudioProcessor>,
    flags: &Flags,
) -> Result<cpal::Stream, EngineError>
where
    T: SizedSample + FromSample<f32> + Send + 'static,
{
    let channels = config.channels as usize;
    let first = p.out_first.min(channels.saturating_sub(1));
    let mut scratch = vec![0f32; 8192 * channels];
    let status = flags.status.clone();
    let rate = config.sample_rate;
    // The processor moves into the callback only if the stream is built;
    // on failure it comes back so a fallback config can try again.
    let slot = Arc::new(Handoff::new(runner.take()));
    let cb_slot = slot.clone();
    let mut proc: Option<Box<AudioProcessor>> = None;
    let flags_err = flags.clone();
    let built = p
        .output
        .build_output_stream::<T, _, _>(
            config,
            move |data: &mut [T], info| {
                let runner = match &mut proc {
                    Some(r) => r,
                    None => {
                        // First callback: take the processor (one swap).
                        let Some(r) = cb_slot.take() else {
                            data.fill(T::EQUILIBRIUM);
                            return;
                        };
                        proc.insert(r)
                    }
                };
                if !runner.mark_thread_promoted() {
                    crate::thread_priority::promote_audio_thread();
                }
                let ts = info.timestamp();
                let frames = (data.len() / channels.max(1)) as u32;
                status.io.publish_output(frames, frames_between(ts.playback, ts.callback, rate));
                runner.begin_callback();
                for chunk in data.chunks_mut(scratch.len()) {
                    let s = &mut scratch[..chunk.len()];
                    runner.process_interleaved_at(s, channels, first);
                    for (d, x) in chunk.iter_mut().zip(s.iter()) {
                        *d = T::from_sample(*x);
                    }
                }
            },
            move |err| flags_err.on_error(err.kind()),
            None,
        )
        .map_err(|e| EngineError::Audio(e.to_string()));
    if built.is_err() {
        *runner = slot.take().map(|b| *b);
    }
    built
}

fn build_input<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    tx: &mut Option<Producer<f32>>,
    flags: &Flags,
) -> Result<cpal::Stream, EngineError>
where
    T: SizedSample + Send + 'static,
    f32: FromSample<T>,
{
    let slot = Arc::new(Handoff::new(tx.take()));
    let cb_slot = slot.clone();
    let mut producer: Option<Box<Producer<f32>>> = None;
    let status = flags.status.clone();
    let flags_err = flags.clone();
    let channels = config.channels.max(1) as usize;
    let rate = config.sample_rate;
    let built = device
        .build_input_stream::<T, _, _>(
            config,
            move |data: &[T], info| {
                let tx = match &mut producer {
                    Some(t) => t,
                    None => {
                        let Some(t) = cb_slot.take() else { return };
                        producer.insert(t)
                    }
                };
                let ts = info.timestamp();
                status.io.publish_input((data.len() / channels) as u32, frames_between(ts.callback, ts.capture, rate));
                for s in data {
                    if tx.push(f32::from_sample(*s)).is_err() {
                        break;
                    }
                }
            },
            move |err| {
                // A vanished input (Bluetooth mic, USB interface) rebuilds
                // the streams so the output keeps running without it.
                if is_fatal(err.kind()) {
                    flags_err.lost.store(true, Ordering::Release);
                }
            },
            None,
        )
        .map_err(|e| EngineError::Audio(e.to_string()));
    if built.is_err() {
        *tx = slot.take().map(|b| *b);
    }
    built
}

fn build_output_any(p: &Prepared, config: cpal::StreamConfig, runner: &mut Option<AudioProcessor>, flags: &Flags) -> Result<cpal::Stream, EngineError> {
    match p.out_format {
        SampleFormat::F32 => build_output::<f32>(p, config, runner, flags),
        SampleFormat::I16 => build_output::<i16>(p, config, runner, flags),
        SampleFormat::I32 => build_output::<i32>(p, config, runner, flags),
        SampleFormat::F64 => build_output::<f64>(p, config, runner, flags),
        SampleFormat::U16 => build_output::<u16>(p, config, runner, flags),
        other => Err(EngineError::Audio(format!("unsupported output sample format {other:?}"))),
    }
}

fn build_input_any(
    dev: &cpal::Device,
    conf: cpal::StreamConfig,
    fmt: SampleFormat,
    tx: &mut Option<Producer<f32>>,
    flags: &Flags,
) -> Result<cpal::Stream, EngineError> {
    match fmt {
        SampleFormat::F32 => build_input::<f32>(dev, conf, tx, flags),
        SampleFormat::I16 => build_input::<i16>(dev, conf, tx, flags),
        SampleFormat::I32 => build_input::<i32>(dev, conf, tx, flags),
        other => Err(EngineError::Audio(format!("unsupported input sample format {other:?}"))),
    }
}

/// Build and start the streams; `runner` moves into the output callback.
pub(crate) fn start(
    p: Prepared,
    runner: AudioProcessor,
    input_tx: Option<Producer<f32>>,
    status: Arc<SharedStatus>,
) -> Result<OpenStream, EngineError> {
    let flags = Flags { status: status.clone(), lost: Arc::new(AtomicBool::new(false)), rerouted: Arc::new(AtomicBool::new(false)) };
    status.io.reset();
    let mut active = p.active.clone();
    let mut runner = Some(runner);
    // A fixed buffer size some drivers refuse (WASAPI shared mode, a few
    // USB/Bluetooth drivers): retry with the driver's own size.
    let out = match build_output_any(&p, p.out_config, &mut runner, &flags) {
        Ok(s) => s,
        Err(e) if matches!(p.out_config.buffer_size, cpal::BufferSize::Fixed(_)) && runner.is_some() => {
            log::info!("audio: fixed buffer refused ({e}); using the driver default");
            active.notes.push("the device refused the buffer size; using its default".into());
            build_output_any(&p, cpal::StreamConfig { buffer_size: cpal::BufferSize::Default, ..p.out_config }, &mut runner, &flags)?
        }
        Err(e) => return Err(e),
    };
    let mut input_tx = input_tx;
    let input = match &p.input {
        Some((dev, conf, fmt)) if input_tx.is_some() => {
            let built = build_input_any(dev, *conf, *fmt, &mut input_tx, &flags).or_else(|e| {
                if matches!(conf.buffer_size, cpal::BufferSize::Fixed(_)) && input_tx.is_some() {
                    build_input_any(dev, cpal::StreamConfig { buffer_size: cpal::BufferSize::Default, ..*conf }, *fmt, &mut input_tx, &flags)
                } else {
                    Err(e)
                }
            });
            match built {
                Ok(s) => {
                    if let Err(e) = s.play() {
                        log::warn!("could not start audio input: {e}");
                    }
                    Some(s)
                }
                Err(e) => {
                    log::warn!("audio input disabled: {e}");
                    None
                }
            }
        }
        _ => None,
    };
    out.play().map_err(|e| EngineError::Audio(e.to_string()))?;
    if let Ok(n) = out.buffer_size() {
        active.buffer_size = Some(n);
    }
    Ok(OpenStream { _out: out, _input: input, lost: flags.lost, rerouted: flags.rerouted, active })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffer_defaults_to_256_and_clamps_to_the_device() {
        let mut notes = Vec::new();
        let range = cpal::SupportedBufferSize::Range { min: 64, max: 2048 };
        assert_eq!(pick_buffer(None, &range, &mut notes), cpal::BufferSize::Fixed(DEFAULT_BUFFER));
        assert_eq!(pick_buffer(Some(128), &range, &mut notes), cpal::BufferSize::Fixed(128));
        assert!(notes.is_empty());
        assert_eq!(pick_buffer(Some(32), &range, &mut notes), cpal::BufferSize::Fixed(64));
        assert_eq!(pick_buffer(Some(8192), &range, &mut notes), cpal::BufferSize::Fixed(2048));
        assert_eq!(notes.len(), 2);
        // A default outside the device range is clamped silently.
        let big = cpal::SupportedBufferSize::Range { min: 512, max: 4096 };
        let mut quiet = Vec::new();
        assert_eq!(pick_buffer(None, &big, &mut quiet), cpal::BufferSize::Fixed(512));
        assert!(quiet.is_empty());
        // Unknown ranges take the request as is; a disjoint range keeps the driver default.
        assert_eq!(pick_buffer(Some(96), &cpal::SupportedBufferSize::Unknown, &mut quiet), cpal::BufferSize::Fixed(96));
        let odd = cpal::SupportedBufferSize::Range { min: 8192, max: 16384 };
        assert_eq!(pick_buffer(Some(256), &odd, &mut quiet), cpal::BufferSize::Default);
    }

    #[test]
    fn frames_between_stream_instants() {
        let cb = cpal::StreamInstant::new(1, 0);
        let play = cpal::StreamInstant::new(1, 10_000_000);
        assert_eq!(frames_between(play, cb, 48_000), 480);
        // Out of order (a driver glitch) saturates to zero.
        assert_eq!(frames_between(cb, play, 48_000), 0);
    }

    #[test]
    fn handoff_moves_once_and_frees_leftovers() {
        let h = Handoff::new(Some(Arc::new(5)));
        let probe = h.take().expect("first take");
        assert_eq!(**probe, 5);
        assert!(h.take().is_none());
        let kept = Arc::new(());
        let h = Handoff::new(Some(kept.clone()));
        assert_eq!(Arc::strong_count(&kept), 2);
        drop(h);
        assert_eq!(Arc::strong_count(&kept), 1, "dropped with the cell");
        assert!(Handoff::<u8>::new(None).take().is_none());
    }

    #[test]
    fn device_errors_raise_the_right_flag() {
        let f = Flags { status: Arc::new(SharedStatus::new()), lost: Arc::default(), rerouted: Arc::default() };
        f.on_error(cpal::ErrorKind::Xrun);
        assert_eq!(f.status.xruns.load(Ordering::Relaxed), 1);
        assert!(!f.lost.load(Ordering::Relaxed) && !f.rerouted.load(Ordering::Relaxed));
        f.on_error(cpal::ErrorKind::DeviceChanged);
        assert!(f.rerouted.load(Ordering::Relaxed) && !f.lost.load(Ordering::Relaxed));
        f.on_error(cpal::ErrorKind::StreamInvalidated);
        assert!(f.lost.load(Ordering::Relaxed));
    }
}
