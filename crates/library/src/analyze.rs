//! Per-file analysis. The scanner calls an [`Analyzer`] for every new or
//! changed file.
//!
//! [`BasicAnalyzer`] ships with the library: audio metadata (duration, rate,
//! channels, peak, RMS) plus the filename/folder hints from [`crate::parse`].
//! The app plugs in a richer analyzer backed by `aitrack-ai` (key/BPM from
//! audio, CLAP category and embeddings) by implementing the same trait,
//! usually wrapping [`BasicAnalyzer`] and overriding the fields it knows
//! better. Bump [`Analyzer::version`] to make the next scan re-analyze
//! everything.

use std::fs::File;
use std::path::Path;

use crate::error::{LibraryError, Result};
use crate::parse::{KeyHint, NameHints};
use crate::taxonomy::{Category, SampleKind, loop_family};

/// Where a musical field came from, most trusted first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Source {
    User,
    Audio,
    #[default]
    Filename,
    Estimate,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Audio => "audio",
            Self::Filename => "filename",
            Self::Estimate => "estimate",
        }
    }
}

/// Decoded audio facts.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AudioInfo {
    pub duration_s: f64,
    pub sample_rate: u32,
    pub channels: u16,
    pub frames: u64,
    /// Sample peak in dBFS (`None` for digital silence).
    pub peak_db: Option<f32>,
    /// RMS level in dBFS over the whole file.
    pub rms_db: Option<f32>,
}

/// The result of analysing one file.
#[derive(Clone, Debug, Default)]
pub struct Analysis {
    pub audio: AudioInfo,
    pub key: Option<KeyHint>,
    pub key_source: Source,
    pub root_note: Option<u8>,
    pub bpm: Option<f32>,
    pub bpm_source: Source,
    pub kind: SampleKind,
    pub loop_family: Option<&'static str>,
    pub category: Option<Category>,
    pub subcategory: Option<String>,
    pub category_source: Source,
    pub genres: Vec<String>,
    /// Integrated loudness. The basic analyzer stores RMS dBFS here
    /// (`loudness_kind = "rms"`); an `aitrack-ai` analyzer stores LUFS.
    pub loudness: Option<f32>,
    pub loudness_kind: &'static str,
    /// Optional embedding (e.g. CLAP) and the model that produced it.
    pub embedding: Option<(String, Vec<f32>)>,
}

/// Something that turns a file into an [`Analysis`]. Must be thread-safe: the
/// scanner runs it on a background thread.
pub trait Analyzer: Send + Sync {
    /// Stable identifier stored with each row (e.g. `"basic"`, `"aitrack-ai"`).
    fn id(&self) -> &str;
    /// Increase to force re-analysis of files analysed by an older version.
    fn version(&self) -> u32;
    /// Analyse `path`. `hints` are the filename/folder hints, already merged
    /// with the pack defaults.
    fn analyze(&self, path: &Path, hints: &NameHints) -> Result<Analysis>;
}

/// Metadata + filename analyzer. Cheap: one decode pass for the peak.
#[derive(Clone, Debug, Default)]
pub struct BasicAnalyzer;

impl BasicAnalyzer {
    pub const VERSION: u32 = 2;
}

impl Analyzer for BasicAnalyzer {
    fn id(&self) -> &str {
        "basic"
    }

    fn version(&self) -> u32 {
        Self::VERSION
    }

    fn analyze(&self, path: &Path, hints: &NameHints) -> Result<Analysis> {
        let audio = read_audio_info(path)?;
        Ok(analysis_from_hints(audio, hints))
    }
}

/// Combines audio facts with name hints: loop detection and BPM estimate.
pub fn analysis_from_hints(audio: AudioInfo, hints: &NameHints) -> Analysis {
    let mut a = Analysis {
        key: hints.key,
        root_note: hints.root_note,
        bpm: hints.bpm,
        category: hints.category,
        subcategory: hints.subcategory.map(str::to_string),
        genres: hints.genres.iter().map(|g| g.to_string()).collect(),
        loudness: audio.rms_db,
        loudness_kind: "rms",
        ..Default::default()
    };
    let dur = audio.duration_s;

    a.kind = match hints.kind {
        Some(k) => k,
        None => {
            // A file whose length is a whole number of beats at its BPM, at
            // least one bar long, is a loop.
            match hints.bpm {
                Some(bpm) if dur > 0.0 => {
                    let beats = dur * bpm as f64 / 60.0;
                    if beats >= 3.8 && (beats - beats.round()).abs() < 0.06 {
                        SampleKind::Loop
                    } else {
                        SampleKind::OneShot
                    }
                }
                _ => SampleKind::OneShot,
            }
        }
    };

    // Loops without a BPM in the name: assume 4/4 and 1, 2, 4, 8 or 16 bars,
    // pick the tempo closest to 120 within 70..180.
    if a.kind == SampleKind::Loop && a.bpm.is_none() && dur > 0.5 {
        a.bpm = estimate_loop_bpm(dur);
        if a.bpm.is_some() {
            a.bpm_source = Source::Estimate;
        }
    }

    if a.kind == SampleKind::Loop {
        a.loop_family = Some(loop_family(a.category, hints.subcategory, hints.top_loop));
    }
    a.audio = audio;
    a
}

/// Tempo of a 4/4 loop from its length alone.
pub fn estimate_loop_bpm(duration_s: f64) -> Option<f32> {
    [1.0, 2.0, 4.0, 8.0, 16.0]
        .iter()
        .map(|bars| bars * 4.0 * 60.0 / duration_s)
        .filter(|b| (70.0..=180.0).contains(b))
        .min_by(|a, b| (a - 120.0).abs().total_cmp(&(b - 120.0).abs()))
        .map(|b| (b * 10.0).round() as f32 / 10.0)
}

fn to_db(x: f64) -> Option<f32> {
    (x > 1e-9).then(|| (20.0 * x.log10()) as f32)
}

/// Reads duration, format and levels. WAV goes through `hound` (fast path);
/// everything else, and WAVs hound rejects, through `symphonia`.
pub fn read_audio_info(path: &Path) -> Result<AudioInfo> {
    let is_wav = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("wav") || e.eq_ignore_ascii_case("wave"));
    if is_wav && let Ok(info) = read_wav(path) {
        return Ok(info);
    }
    read_symphonia(path)
}

fn read_wav(path: &Path) -> Result<AudioInfo> {
    let mut r = hound::WavReader::open(path).map_err(|e| LibraryError::Audio(e.to_string()))?;
    let spec = r.spec();
    let mut peak = 0f64;
    let mut sum_sq = 0f64;
    let mut n = 0u64;
    match spec.sample_format {
        hound::SampleFormat::Float => {
            for s in r.samples::<f32>() {
                let v = s.map_err(|e| LibraryError::Audio(e.to_string()))? as f64;
                peak = peak.max(v.abs());
                sum_sq += v * v;
                n += 1;
            }
        }
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample.clamp(1, 32) - 1)) as f64;
            for s in r.samples::<i32>() {
                let v = s.map_err(|e| LibraryError::Audio(e.to_string()))? as f64 * scale;
                peak = peak.max(v.abs());
                sum_sq += v * v;
                n += 1;
            }
        }
    }
    let ch = spec.channels.max(1);
    let frames = n / ch as u64;
    Ok(AudioInfo {
        duration_s: frames as f64 / spec.sample_rate.max(1) as f64,
        sample_rate: spec.sample_rate,
        channels: ch,
        frames,
        peak_db: to_db(peak),
        rms_db: (n > 0).then(|| to_db((sum_sq / n as f64).sqrt())).flatten(),
    })
}

fn read_symphonia(path: &Path) -> Result<AudioInfo> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::{CODEC_TYPE_NULL, DecoderOptions};
    use symphonia::core::errors::Error as SymErr;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let audio_err = |e: SymErr| LibraryError::Audio(e.to_string());
    let file = File::open(path)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(audio_err)?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| LibraryError::Audio("no audio track".into()))?;
    let track_id = track.id;
    let params = track.codec_params.clone();
    let mut decoder = symphonia::default::get_codecs()
        .make(&params, &DecoderOptions::default())
        .map_err(audio_err)?;

    let mut rate = params.sample_rate.unwrap_or(0);
    let mut channels = params.channels.map_or(0, |c| c.count() as u16);
    let mut peak = 0f64;
    let mut sum_sq = 0f64;
    let mut n = 0u64;
    let mut frames = 0u64;
    let mut buf: Option<SampleBuffer<f32>> = None;
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymErr::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(SymErr::ResetRequired) => break,
            Err(e) => return Err(audio_err(e)),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(SymErr::DecodeError(_)) => continue,
            Err(e) => return Err(audio_err(e)),
        };
        let spec = *decoded.spec();
        rate = spec.rate;
        channels = spec.channels.count() as u16;
        frames += decoded.frames() as u64;
        let needed = decoded.capacity() as u64;
        let sb = match &mut buf {
            Some(b) if b.capacity() as u64 >= needed => b,
            _ => buf.insert(SampleBuffer::<f32>::new(needed, spec)),
        };
        sb.copy_interleaved_ref(decoded);
        for &s in sb.samples() {
            let v = s as f64;
            peak = peak.max(v.abs());
            sum_sq += v * v;
            n += 1;
        }
    }
    if frames == 0 {
        frames = params.n_frames.unwrap_or(0);
    }
    if rate == 0 {
        return Err(LibraryError::Audio("unknown sample rate".into()));
    }
    Ok(AudioInfo {
        duration_s: frames as f64 / rate as f64,
        sample_rate: rate,
        channels: channels.max(1),
        frames,
        peak_db: to_db(peak),
        rms_db: (n > 0).then(|| to_db((sum_sq / n as f64).sqrt())).flatten(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_path;
    use crate::test_util::write_wav;

    #[test]
    fn wav_metadata_and_loop_detection() {
        let dir = tempfile::tempdir().unwrap();
        // 2 bars at 120 BPM = 4 s.
        let p = dir.path().join("Drum Groove 120.wav");
        write_wav(&p, 44_100, 2, 4.0, 0.5);
        let hints = parse_path(&p, Some(dir.path()));
        let a = BasicAnalyzer.analyze(&p, &hints).unwrap();
        assert_eq!(a.audio.sample_rate, 44_100);
        assert_eq!(a.audio.channels, 2);
        assert!((a.audio.duration_s - 4.0).abs() < 1e-3);
        let peak = a.audio.peak_db.unwrap();
        assert!((peak - (-6.02)).abs() < 0.1, "peak {peak}");
        assert_eq!(a.kind, SampleKind::Loop);
        assert_eq!(a.bpm, Some(120.0));
        assert_eq!(a.loop_family, Some("Drum"));

        // A loop with a BPM-free name gets an estimate from its length.
        let p2 = dir.path().join("Perc Loop.wav");
        write_wav(&p2, 48_000, 1, 1.875, 0.25); // 1 bar at 128
        let a2 = BasicAnalyzer.analyze(&p2, &parse_path(&p2, Some(dir.path()))).unwrap();
        assert_eq!(a2.kind, SampleKind::Loop);
        assert_eq!(a2.bpm, Some(128.0));
        assert_eq!(a2.bpm_source, Source::Estimate);

        // A short kick is a one-shot.
        let p3 = dir.path().join("Kick 1.wav");
        write_wav(&p3, 44_100, 1, 0.3, 0.9);
        let a3 = BasicAnalyzer.analyze(&p3, &parse_path(&p3, None)).unwrap();
        assert_eq!(a3.kind, SampleKind::OneShot);
        assert_eq!(a3.category, Some(Category::Drums));
    }

    #[test]
    fn bpm_estimate() {
        assert_eq!(estimate_loop_bpm(2.0), Some(120.0));
        assert_eq!(estimate_loop_bpm(1.875), Some(128.0));
        assert_eq!(estimate_loop_bpm(0.01), None);
    }
}
