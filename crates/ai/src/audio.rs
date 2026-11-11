//! Decoding (symphonia), downmixing and resampling (rubato), WAV writing (hound).

use std::fs::File;
use std::path::Path;

use rubato::{
    FftFixedIn, Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType,
    WindowFunction,
};
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CODEC_TYPE_NULL, DecoderOptions};
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use crate::{Error, Result};

/// Decoded audio, planar (one `Vec` per channel).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AudioData {
    pub channels: Vec<Vec<f32>>,
    pub sample_rate: u32,
}

impl AudioData {
    pub fn new(channels: Vec<Vec<f32>>, sample_rate: u32) -> Self {
        Self {
            channels,
            sample_rate,
        }
    }

    pub fn mono_signal(samples: Vec<f32>, sample_rate: u32) -> Self {
        Self {
            channels: vec![samples],
            sample_rate,
        }
    }

    pub fn channel_count(&self) -> usize {
        self.channels.len()
    }

    pub fn frames(&self) -> usize {
        self.channels.first().map_or(0, Vec::len)
    }

    pub fn duration(&self) -> f64 {
        if self.sample_rate == 0 {
            0.0
        } else {
            self.frames() as f64 / self.sample_rate as f64
        }
    }

    /// Average of all channels.
    pub fn to_mono(&self) -> Vec<f32> {
        downmix(&self.channels)
    }

    /// Exactly two channels: mono is duplicated, extra channels are dropped.
    pub fn to_stereo(&self) -> [Vec<f32>; 2] {
        match self.channels.len() {
            0 => [Vec::new(), Vec::new()],
            1 => [self.channels[0].clone(), self.channels[0].clone()],
            _ => [self.channels[0].clone(), self.channels[1].clone()],
        }
    }

    /// Resampled copy (all channels).
    pub fn resampled(&self, rate: u32) -> Result<Self> {
        Ok(Self {
            channels: resample(&self.channels, self.sample_rate, rate)?,
            sample_rate: rate,
        })
    }
}

pub fn downmix(channels: &[Vec<f32>]) -> Vec<f32> {
    match channels.len() {
        0 => Vec::new(),
        1 => channels[0].clone(),
        n => {
            let len = channels.iter().map(Vec::len).min().unwrap_or(0);
            let scale = 1.0 / n as f32;
            (0..len)
                .map(|i| channels.iter().map(|c| c[i]).sum::<f32>() * scale)
                .collect()
        }
    }
}

/// Decode any format symphonia supports (WAV, AIFF, MP3, FLAC, OGG/Vorbis, AAC/ALAC in MP4/M4A).
pub fn load(path: impl AsRef<Path>) -> Result<AudioData> {
    let path = path.as_ref();
    let file = File::open(path).map_err(|e| Error::io(path, e))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| Error::Decode(format!("{}: {e}", path.display())))?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| Error::Decode("no audio track".into()))?;
    let track_id = track.id;
    let mut sample_rate = track.codec_params.sample_rate.unwrap_or(0);
    let mut channel_count = track.codec_params.channels.map_or(0, |c| c.count());
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| Error::Decode(e.to_string()))?;

    let mut channels: Vec<Vec<f32>> = vec![Vec::new(); channel_count];
    let mut buf: Option<SampleBuffer<f32>> = None;
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymError::ResetRequired) => break,
            Err(e) => return Err(Error::Decode(e.to_string())),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            // Corrupt frames are skipped, as players do.
            Err(SymError::DecodeError(_)) => continue,
            Err(e) => return Err(Error::Decode(e.to_string())),
        };
        let spec = *decoded.spec();
        if channel_count == 0 || channels.len() != spec.channels.count() {
            channel_count = spec.channels.count();
            channels = vec![Vec::new(); channel_count];
        }
        sample_rate = spec.rate;
        let needed = decoded.capacity() as u64;
        if buf
            .as_ref()
            .is_none_or(|b| b.capacity() < decoded.capacity())
        {
            buf = Some(SampleBuffer::new(needed, spec));
        }
        let b = buf.as_mut().expect("buffer allocated above");
        b.copy_interleaved_ref(decoded);
        for frame in b.samples().chunks_exact(channel_count) {
            for (c, s) in frame.iter().enumerate() {
                channels[c].push(*s);
            }
        }
    }
    if sample_rate == 0 || channels.is_empty() {
        return Err(Error::Decode(format!(
            "{}: no decodable audio",
            path.display()
        )));
    }
    Ok(AudioData {
        channels,
        sample_rate,
    })
}

/// Load, downmix to mono and resample to `rate`.
pub fn load_mono(path: impl AsRef<Path>, rate: u32) -> Result<Vec<f32>> {
    let audio = load(path)?;
    let mono = audio.to_mono();
    resample_channel(&mono, audio.sample_rate, rate)
}

pub fn resample_channel(x: &[f32], from: u32, to: u32) -> Result<Vec<f32>> {
    Ok(resample(&[x.to_vec()], from, to)?.pop().unwrap_or_default())
}

/// High-quality FFT resampling. Output length is `round(len * to / from)` and
/// the resampler delay is compensated, so the output stays time-aligned.
pub fn resample(channels: &[Vec<f32>], from: u32, to: u32) -> Result<Vec<Vec<f32>>> {
    if from == to || channels.is_empty() {
        return Ok(channels.to_vec());
    }
    if from == 0 || to == 0 {
        return Err(Error::InvalidInput("sample rate 0".into()));
    }
    let rs = FftFixedIn::<f32>::new(from as usize, to as usize, 1024, 2, channels.len())
        .map_err(|e| Error::InvalidInput(e.to_string()))?;
    let delay = rs.output_delay();
    run_resampler(rs, delay, channels, from, to)
}

/// Band-limited sinc resampling (256-tap Blackman-Harris-squared kernel, cubic
/// interpolation): the highest-quality path, used for stems written back to
/// the source rate. Same length and alignment guarantees as [`resample`].
pub fn resample_sinc(channels: &[Vec<f32>], from: u32, to: u32) -> Result<Vec<Vec<f32>>> {
    if from == to || channels.is_empty() {
        return Ok(channels.to_vec());
    }
    if from == 0 || to == 0 {
        return Err(Error::InvalidInput("sample rate 0".into()));
    }
    let params = SincInterpolationParameters {
        sinc_len: 256,
        f_cutoff: 0.95,
        interpolation: SincInterpolationType::Cubic,
        oversampling_factor: 256,
        window: WindowFunction::BlackmanHarris2,
    };
    let rs = SincFixedIn::<f32>::new(to as f64 / from as f64, 1.0, params, 1024, channels.len())
        .map_err(|e| Error::InvalidInput(e.to_string()))?;
    // rubato's sinc resampler starts its read position half a kernel early, so its output is
    // already aligned (measured with impulses at 0.5x, 0.919x, 1.088x and 2x); its
    // `output_delay()` must not be compensated a second time.
    run_resampler(rs, 0, channels, from, to)
}

fn run_resampler<R: Resampler<f32>>(
    mut rs: R,
    delay: usize,
    channels: &[Vec<f32>],
    from: u32,
    to: u32,
) -> Result<Vec<Vec<f32>>> {
    let n = channels.iter().map(Vec::len).min().unwrap_or(0);
    let expected = ((n as u64 * to as u64 + from as u64 / 2) / from as u64) as usize;
    if n == 0 {
        return Ok(vec![Vec::new(); channels.len()]);
    }
    let mut out: Vec<Vec<f32>> = vec![Vec::with_capacity(expected + delay + 4096); channels.len()];
    let mut pos = 0;
    let mut block: Vec<Vec<f32>> = Vec::new();
    while out[0].len() < expected + delay {
        let chunk = rs.input_frames_next();
        block.resize(channels.len(), Vec::new());
        for (c, ch) in channels.iter().enumerate() {
            block[c].clear();
            block[c].extend((0..chunk).map(|i| ch.get(pos + i).copied().unwrap_or(0.0)));
        }
        pos += chunk;
        let res = rs
            .process(&block, None)
            .map_err(|e| Error::InvalidInput(e.to_string()))?;
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

/// Sample format for written WAV files.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WavFormat {
    Pcm16,
    #[default]
    Pcm24,
    Float32,
}

pub fn write_wav(
    path: impl AsRef<Path>,
    channels: &[Vec<f32>],
    sample_rate: u32,
    format: WavFormat,
) -> Result<()> {
    let path = path.as_ref();
    let (bits, fmt) = match format {
        WavFormat::Pcm16 => (16, hound::SampleFormat::Int),
        WavFormat::Pcm24 => (24, hound::SampleFormat::Int),
        WavFormat::Float32 => (32, hound::SampleFormat::Float),
    };
    let spec = hound::WavSpec {
        channels: channels.len() as u16,
        sample_rate,
        bits_per_sample: bits,
        sample_format: fmt,
    };
    let to_io = |e: hound::Error| match e {
        hound::Error::IoError(io) => Error::io(path, io),
        other => Error::InvalidInput(other.to_string()),
    };
    let mut w = hound::WavWriter::create(path, spec).map_err(to_io)?;
    let frames = channels.iter().map(Vec::len).min().unwrap_or(0);
    for i in 0..frames {
        for ch in channels {
            let s = ch[i];
            match format {
                WavFormat::Float32 => w.write_sample(s).map_err(to_io)?,
                WavFormat::Pcm16 => w
                    .write_sample((s.clamp(-1.0, 1.0) * 32767.0).round() as i16)
                    .map_err(to_io)?,
                WavFormat::Pcm24 => w
                    .write_sample((s.clamp(-1.0, 1.0) * 8_388_607.0).round() as i32)
                    .map_err(to_io)?,
            }
        }
    }
    w.finalize().map_err(to_io)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_keeps_length_and_pitch() {
        let sr = 44_100;
        let x: Vec<f32> = (0..sr)
            .map(|i| (2.0 * std::f32::consts::PI * 440.0 * i as f32 / sr as f32).sin())
            .collect();
        let y = resample_channel(&x, sr, 22_050).unwrap();
        assert_eq!(y.len(), 22_050);
        // Zero crossings of a 440 Hz sine over one second ≈ 880.
        let zc = y
            .windows(2)
            .filter(|w| (w[0] >= 0.0) != (w[1] >= 0.0))
            .count();
        assert!((870..=890).contains(&zc), "zero crossings {zc}");
        // Aligned: the 0.25 s sample matches the analytic value.
        let i = 5_000;
        let expect = (2.0 * std::f32::consts::PI * 440.0 * i as f32 / 22_050.0).sin();
        assert!((y[i] - expect).abs() < 0.01, "{} vs {expect}", y[i]);
    }

    #[test]
    fn sinc_resample_round_trip_is_aligned() {
        let sr = 44_100;
        let x: Vec<f32> = (0..48_000)
            .map(|i| (2.0 * std::f32::consts::PI * 997.0 * i as f32 / sr as f32).sin() * 0.5)
            .collect();
        let up = resample_sinc(std::slice::from_ref(&x), 44_100, 48_000).unwrap();
        let back = resample_sinc(&up, 48_000, 44_100).unwrap().pop().unwrap();
        assert_eq!(back.len(), x.len());
        let err = x[2000..46_000]
            .iter()
            .zip(&back[2000..46_000])
            .map(|(a, b)| (a - b).abs())
            .fold(0.0f32, f32::max);
        assert!(err < 2e-3, "max error {err}");
        // Impulse lands where it should after a 44.1 -> 48 kHz conversion.
        let mut imp = vec![0.0f32; 40_000];
        imp[20_000] = 1.0;
        let o = resample_sinc(&[imp], 44_100, 48_000)
            .unwrap()
            .pop()
            .unwrap();
        let peak = o
            .iter()
            .enumerate()
            .fold(
                (0, 0.0f32),
                |a, (i, v)| if v.abs() > a.1 { (i, v.abs()) } else { a },
            )
            .0;
        assert!(
            (peak as f32 - 20_000.0 * 48_000.0 / 44_100.0).abs() <= 1.0,
            "peak {peak}"
        );
    }

    #[test]
    fn wav_round_trip() {
        let dir = std::env::temp_dir().join(format!("aitrack-ai-wav-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t.wav");
        let x: Vec<f32> = (0..1000).map(|i| (i as f32 * 0.01).sin() * 0.5).collect();
        write_wav(&p, &[x.clone(), x.clone()], 48_000, WavFormat::Pcm24).unwrap();
        let a = load(&p).unwrap();
        assert_eq!(a.sample_rate, 48_000);
        assert_eq!(a.channel_count(), 2);
        assert_eq!(a.frames(), 1000);
        assert!((a.channels[1][500] - x[500]).abs() < 1e-4);
        std::fs::remove_dir_all(&dir).ok();
    }
}
