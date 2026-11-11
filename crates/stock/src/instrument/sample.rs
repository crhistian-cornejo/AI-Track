//! In-memory audio samples for the sampler and the drum sampler: a tiny WAV
//! reader (via `hound`) and a cubic-Hermite reader for fractional playback.
//!
//! Loading and decoding run on the control thread (`load_state`, explicit
//! `load_*` calls). The audio thread only reads.

use aitrack_dsp::delay::hermite;

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
