//! Wavetable WAV import and export (control thread).
//!
//! * Serum / Vital tables: a WAV whose `clm ` chunk reads
//!   `<!>2048 …` gives the frame length (Serum writes 2048; other values are
//!   accepted and resampled to 2048).
//! * Surge tables: a `srge` chunk with `{version: i32, frame_len: i32}`.
//! * No chunk: a length that is a multiple of 2048 is cut into 2048-sample
//!   frames; anything shorter than two frames is one single cycle of
//!   whatever length (AKWF-style single-cycle WAVs); a longer file is cut
//!   into 2048-sample frames and the remainder dropped.
//!
//! Multichannel files are mixed to mono. At most 256 frames are kept.
//! Export writes 32-bit float with a `clm ` chunk, which Serum and Vital
//! read back.

use super::tablegen::WaveFrames;
use super::table::{FRAME_LEN, MAX_FRAMES};
use std::io::Cursor;
use std::path::Path;

#[derive(Debug)]
pub enum WavError {
    Io(std::io::Error),
    Decode(String),
    Empty,
}

impl std::fmt::Display for WavError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WavError::Io(e) => write!(f, "cannot read wavetable: {e}"),
            WavError::Decode(e) => write!(f, "not a usable WAV: {e}"),
            WavError::Empty => write!(f, "the WAV holds no audio"),
        }
    }
}

impl std::error::Error for WavError {}

/// Frame length declared by a `clm ` or `srge` chunk, if any.
pub fn declared_frame_len(bytes: &[u8]) -> Option<usize> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return None;
    }
    let mut i = 12;
    while i + 8 <= bytes.len() {
        let id = &bytes[i..i + 4];
        let len = u32::from_le_bytes([bytes[i + 4], bytes[i + 5], bytes[i + 6], bytes[i + 7]]) as usize;
        let body = &bytes[i + 8..(i + 8 + len).min(bytes.len())];
        if id == b"clm " {
            let text = String::from_utf8_lossy(body);
            if let Some(rest) = text.find("<!>").map(|p| &text[p + 3..]) {
                let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                if let Ok(n) = digits.parse::<usize>()
                    && (16..=65_536).contains(&n)
                {
                    return Some(n);
                }
            }
        } else if id == b"srge" && body.len() >= 8 {
            let n = i32::from_le_bytes([body[4], body[5], body[6], body[7]]);
            if (16..=65_536).contains(&n) {
                return Some(n as usize);
            }
        }
        i += 8 + len + (len & 1);
    }
    None
}

/// Mono samples of a WAV in memory.
pub fn decode_mono(bytes: &[u8]) -> Result<Vec<f32>, WavError> {
    let reader = hound::WavReader::new(Cursor::new(bytes)).map_err(|e| WavError::Decode(e.to_string()))?;
    let spec = reader.spec();
    let ch = spec.channels.max(1) as usize;
    let interleaved: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.into_samples::<f32>().collect::<Result<_, _>>().map_err(|e| WavError::Decode(e.to_string()))?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample.clamp(8, 32) - 1)) as f32;
            reader.into_samples::<i32>().map(|s| s.map(|v| v as f32 * scale)).collect::<Result<_, _>>().map_err(|e| WavError::Decode(e.to_string()))?
        }
    };
    Ok(interleaved.chunks(ch).map(|c| c.iter().sum::<f32>() / ch as f32).collect())
}

/// Import a wavetable WAV from memory.
pub fn import(bytes: &[u8], name: impl Into<String>) -> Result<WaveFrames, WavError> {
    import_with(bytes, name, None)
}

/// Import with a known frame length (e.g. 256 for WaveEdit / E352 banks,
/// which carry no chunk), overriding any chunk and the guess.
pub fn import_with(bytes: &[u8], name: impl Into<String>, frame_len: Option<usize>) -> Result<WaveFrames, WavError> {
    let samples = decode_mono(bytes)?;
    if samples.is_empty() {
        return Err(WavError::Empty);
    }
    let frame = match frame_len.filter(|&n| n >= 16).or_else(|| declared_frame_len(bytes)) {
        Some(n) => n,
        None if samples.len() % FRAME_LEN == 0 => FRAME_LEN,
        None if samples.len() < 2 * FRAME_LEN => samples.len(),
        None => FRAME_LEN,
    };
    let cycles: Vec<Vec<f32>> = samples.chunks_exact(frame.min(samples.len())).take(MAX_FRAMES).map(|c| c.to_vec()).collect();
    if cycles.is_empty() {
        return Err(WavError::Empty);
    }
    Ok(WaveFrames::new(name, cycles))
}

/// Import a wavetable WAV file; the table is named after the file.
pub fn import_file(path: impl AsRef<Path>) -> Result<WaveFrames, WavError> {
    let path = path.as_ref();
    let bytes = std::fs::read(path).map_err(WavError::Io)?;
    let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Wavetable".into());
    import(&bytes, name)
}

/// A 32-bit float mono WAV with a Serum-style `clm ` chunk.
pub fn export(frames: &WaveFrames) -> Vec<u8> {
    let samples: Vec<f32> = frames.frames.iter().flatten().copied().collect();
    let clm = format!("<!>{FRAME_LEN} 10000000 wavetable (AI-Track)");
    let mut clm_body = clm.into_bytes();
    if clm_body.len() % 2 == 1 {
        clm_body.push(0);
    }
    let data_len = samples.len() * 4;
    let fmt_len = 16;
    let riff_len = 4 + (8 + fmt_len) + (8 + clm_body.len()) + (8 + data_len);
    let mut out = Vec::with_capacity(riff_len + 8);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(riff_len as u32).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&(fmt_len as u32).to_le_bytes());
    out.extend_from_slice(&3u16.to_le_bytes()); // IEEE float
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&44_100u32.to_le_bytes());
    out.extend_from_slice(&(44_100u32 * 4).to_le_bytes());
    out.extend_from_slice(&4u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(b"clm ");
    out.extend_from_slice(&(clm_body.len() as u32).to_le_bytes());
    out.extend_from_slice(&clm_body);
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data_len as u32).to_le_bytes());
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A 16-bit PCM WAV with an optional extra chunk before `data`.
    pub(crate) fn pcm16(samples: &[f32], chunk: Option<(&[u8; 4], Vec<u8>)>) -> Vec<u8> {
        let mut extra = Vec::new();
        if let Some((id, mut body)) = chunk {
            if body.len() % 2 == 1 {
                body.push(0);
            }
            extra.extend_from_slice(id);
            extra.extend_from_slice(&(body.len() as u32).to_le_bytes());
            extra.extend_from_slice(&body);
        }
        let data_len = samples.len() * 2;
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&((4 + 24 + extra.len() + 8 + data_len) as u32).to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&48_000u32.to_le_bytes());
        out.extend_from_slice(&96_000u32.to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(&extra);
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data_len as u32).to_le_bytes());
        for s in samples {
            out.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32_767.0) as i16).to_le_bytes());
        }
        out
    }

    fn ramp_frames(frame: usize, n: usize) -> Vec<f32> {
        // Frame f is a saw scaled by (f + 1) / n.
        (0..n).flat_map(|f| (0..frame).map(move |i| (2.0 * i as f32 / frame as f32 - 1.0) * (f + 1) as f32 / n as f32)).collect()
    }

    #[test]
    fn reads_the_clm_chunk() {
        // 4 frames of 1024 samples, declared by clm: a reader assuming 2048
        // would see 2 frames.
        let samples = ramp_frames(1024, 4);
        let bytes = pcm16(&samples, Some((b"clm ", b"<!>1024 10000000 wavetable (www.xferrecords.com)".to_vec())));
        assert_eq!(declared_frame_len(&bytes), Some(1024));
        let w = import(&bytes, "t").unwrap();
        assert_eq!(w.len(), 4);
        assert_eq!(w.frames[0].len(), FRAME_LEN);
        // Each frame keeps its level (resampled saw of peak (f+1)/4).
        let peak = |f: &Vec<f32>| f.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(peak(&w.frames[0]) < 0.4 && peak(&w.frames[3]) > 0.9, "{} {}", peak(&w.frames[0]), peak(&w.frames[3]));
    }

    #[test]
    fn serum_default_and_single_cycles() {
        let samples = ramp_frames(2048, 3);
        let w = import(&pcm16(&samples, None), "t").unwrap();
        assert_eq!(w.len(), 3);
        // AKWF-style 600-sample single cycle.
        let one: Vec<f32> = (0..600).map(|i| (std::f32::consts::TAU * i as f32 / 600.0).sin()).collect();
        let w = import(&pcm16(&one, None), "t").unwrap();
        assert_eq!(w.len(), 1);
        assert!((w.frames[0][512] - 1.0).abs() < 0.01);
        // Surge chunk.
        let mut body = 1i32.to_le_bytes().to_vec();
        body.extend_from_slice(&512i32.to_le_bytes());
        let bytes = pcm16(&ramp_frames(512, 6), Some((b"srge", body)));
        assert_eq!(import(&bytes, "t").unwrap().len(), 6);
        assert!(import(b"nope", "t").is_err());
        // WaveEdit bank: 64 × 256, no chunk; the caller names the length.
        let bank = pcm16(&ramp_frames(256, 64), None);
        assert_eq!(import(&bank, "t").unwrap().len(), 8);
        assert_eq!(import_with(&bank, "t", Some(256)).unwrap().len(), 64);
    }

    #[test]
    fn export_round_trips() {
        let w = super::super::tablegen::factory_table(0).morph(8, super::super::tablegen::Morph::Crossfade);
        let bytes = export(&w);
        assert_eq!(declared_frame_len(&bytes), Some(FRAME_LEN));
        let back = import(&bytes, "t").unwrap();
        assert_eq!(back.len(), 8);
        let err = back.frames[5].iter().zip(&w.frames[5]).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(err < 1e-6);
    }
}
