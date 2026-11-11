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
//! device is gone set a flag the control thread polls to reopen a device.

use crate::rt::AudioProcessor;
use crate::status::SharedStatus;
use crate::EngineError;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample};
use rtrb::Producer;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
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
}

pub const MIN_BUFFER: u32 = 32;
pub const MAX_BUFFER: u32 = 4096;

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

fn supports_rate(configs: &[cpal::SupportedStreamConfigRange], rate: u32) -> bool {
    configs.iter().any(|c| rate >= c.min_sample_rate() && rate <= c.max_sample_rate())
}

fn pick_buffer(requested: Option<u32>, range: &cpal::SupportedBufferSize, notes: &mut Vec<String>) -> cpal::BufferSize {
    let Some(n) = requested else { return cpal::BufferSize::Default };
    let (lo, hi) = match range {
        cpal::SupportedBufferSize::Range { min, max } => ((*min).max(MIN_BUFFER), (*max).min(MAX_BUFFER)),
        cpal::SupportedBufferSize::Unknown => (MIN_BUFFER, MAX_BUFFER),
    };
    let clamped = if lo <= hi { n.clamp(lo, hi) } else { n.clamp(MIN_BUFFER, MAX_BUFFER) };
    if clamped != n {
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
    let output = match find_device(&host, cfg.output_device.as_deref(), cfg.output_device_name.as_deref(), true) {
        Some(d) => d,
        None => {
            if wanted_out {
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
                        let conf = cpal::StreamConfig { channels: ch, sample_rate, buffer_size: cpal::BufferSize::Default };
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
    pub lost: Arc<AtomicBool>,
    pub active: ActiveAudio,
}

fn is_fatal(kind: cpal::ErrorKind) -> bool {
    matches!(
        kind,
        cpal::ErrorKind::DeviceNotAvailable | cpal::ErrorKind::StreamInvalidated | cpal::ErrorKind::BackendError
    )
}

fn build_output<T>(
    p: &Prepared,
    mut runner: AudioProcessor,
    status: Arc<SharedStatus>,
    lost: Arc<AtomicBool>,
) -> Result<cpal::Stream, EngineError>
where
    T: SizedSample + FromSample<f32> + Send + 'static,
{
    let channels = p.out_config.channels as usize;
    let first = p.out_first.min(channels.saturating_sub(1));
    let mut scratch = vec![0f32; 8192 * channels];
    let status_err = status.clone();
    p.output
        .build_output_stream::<T, _, _>(
            p.out_config,
            move |data: &mut [T], _info| {
                if !runner.mark_thread_promoted() {
                    crate::thread_priority::promote_audio_thread();
                }
                for chunk in data.chunks_mut(scratch.len()) {
                    let s = &mut scratch[..chunk.len()];
                    runner.process_interleaved_at(s, channels, first);
                    for (d, x) in chunk.iter_mut().zip(s.iter()) {
                        *d = T::from_sample(*x);
                    }
                }
            },
            move |err| {
                if err.kind() == cpal::ErrorKind::Xrun {
                    status_err.xruns.fetch_add(1, Ordering::Relaxed);
                } else if is_fatal(err.kind()) {
                    lost.store(true, Ordering::Release);
                }
            },
            None,
        )
        .map_err(|e| EngineError::Audio(e.to_string()))
}

fn build_input<T>(device: &cpal::Device, config: cpal::StreamConfig, mut tx: Producer<f32>) -> Result<cpal::Stream, EngineError>
where
    T: SizedSample + Send + 'static,
    f32: FromSample<T>,
{
    device
        .build_input_stream::<T, _, _>(
            config,
            move |data: &[T], _info| {
                for s in data {
                    if tx.push(f32::from_sample(*s)).is_err() {
                        break;
                    }
                }
            },
            |err| log::warn!("audio input error: {err}"),
            None,
        )
        .map_err(|e| EngineError::Audio(e.to_string()))
}

/// Build and start the streams; `runner` moves into the output callback.
pub(crate) fn start(
    p: Prepared,
    runner: AudioProcessor,
    input_tx: Option<Producer<f32>>,
    status: Arc<SharedStatus>,
) -> Result<OpenStream, EngineError> {
    let lost = Arc::new(AtomicBool::new(false));
    let out = match p.out_format {
        SampleFormat::F32 => build_output::<f32>(&p, runner, status, lost.clone()),
        SampleFormat::I16 => build_output::<i16>(&p, runner, status, lost.clone()),
        SampleFormat::I32 => build_output::<i32>(&p, runner, status, lost.clone()),
        SampleFormat::F64 => build_output::<f64>(&p, runner, status, lost.clone()),
        SampleFormat::U16 => build_output::<u16>(&p, runner, status, lost.clone()),
        other => Err(EngineError::Audio(format!("unsupported output sample format {other:?}"))),
    }?;
    let input = match (&p.input, input_tx) {
        (Some((dev, conf, fmt)), Some(tx)) => {
            let built = match fmt {
                SampleFormat::F32 => build_input::<f32>(dev, *conf, tx),
                SampleFormat::I16 => build_input::<i16>(dev, *conf, tx),
                SampleFormat::I32 => build_input::<i32>(dev, *conf, tx),
                other => Err(EngineError::Audio(format!("unsupported input sample format {other:?}"))),
            };
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
    let mut active = p.active;
    if let Ok(n) = out.buffer_size() {
        active.buffer_size = Some(n);
    }
    Ok(OpenStream { _out: out, _input: input, lost, active })
}
