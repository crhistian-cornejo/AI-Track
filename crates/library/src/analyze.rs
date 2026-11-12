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
    /// Attacks heard in the file (see [`OnsetCounter`]); `None` when the
    /// audio was not decoded (a re-tag from stored rows).
    pub onsets: Option<u32>,
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
    /// 3: whole-word genres, no pack-wide genres per sample, onset-aware
    /// loop/one-shot classification.
    pub const VERSION: u32 = 3;
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

    let (kind, guessed_bpm) = classify_kind(hints, dur, audio.onsets);
    a.kind = kind;
    if a.bpm.is_none()
        && let Some(b) = guessed_bpm
    {
        a.bpm = Some(b);
    }

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

/// Fewer beats than this are not a loop, whatever the name's tempo says.
const MIN_LOOP_BEATS: f64 = 3.8;
/// Shorter files are never called loops by the audio analysis alone.
const MIN_ANALYSED_LOOP_S: f64 = 2.0;
/// Attacks a drum file needs before the audio analysis calls it a loop.
const MIN_LOOP_ONSETS: u32 = 4;

/// Single drum hits and FX gestures: a one-shot unless the name says loop
/// or gives a tempo the length matches.
fn is_hit_like(hints: &NameHints) -> bool {
    matches!(
        (hints.category, hints.subcategory),
        (Some(Category::Drums), Some("Kicks" | "Snares" | "Claps" | "Hats" | "Cymbals" | "808s"))
            | (Some(Category::Fx), Some("Impacts" | "Risers" | "Sweeps"))
    )
}

/// Whether `dur` holds a whole number of beats at `bpm` (at least about a
/// bar). With `bars_only` the count must also be a whole number of 4/4 bars,
/// which is what an untrusted number in a name has to satisfy.
fn fits_tempo(dur: f64, bpm: f32, bars_only: bool) -> bool {
    if dur <= 0.0 || bpm <= 0.0 {
        return false;
    }
    let beats = dur * bpm as f64 / 60.0;
    if beats < MIN_LOOP_BEATS {
        return false;
    }
    // Trimmed or padded exports drift a little: allow 1.5 % or 0.06 beats.
    let tol = (beats * 0.015).max(0.06);
    let unit = if bars_only { 4.0 } else { 1.0 };
    let n = (beats / unit).round() * unit;
    n > 0.0 && (beats - n).abs() <= tol
}

/// One-shot or loop, in a fixed order of trust:
///
/// 1. **The name says so**: `loop`/`lp`, `one shot`/`os`/`hit`, `chop`, in
///    the file name or (failing that) its nearest folder.
/// 2. **A tempo in the name** (`128bpm`) and a length that is a whole number
///    of beats at it, at least a bar: a loop.
/// 3. **Structure**: a drum hit (kick, snare, hat…), an FX gesture (impact,
///    riser) or a multisample note (`a0`, `C4v8`) is a one-shot.
/// 4. **A bare number** in the name (`Perc_128`) that the length matches as
///    whole bars: a loop at that tempo.
/// 5. **Audio**: a drum file without a specific piece, or one whose name
///    hints at a loop (`beat`, `riff`), at least two seconds long with
///    several attacks: a loop.
///
/// Everything else is a one-shot. Returns the kind and, for case 4, the
/// tempo that the name's number turned out to be.
pub fn classify_kind(hints: &NameHints, dur: f64, onsets: Option<u32>) -> (SampleKind, Option<f32>) {
    if let Some(k) = hints.kind {
        return (k, None);
    }
    if let Some(bpm) = hints.bpm {
        return (if fits_tempo(dur, bpm, false) { SampleKind::Loop } else { SampleKind::OneShot }, None);
    }
    if is_hit_like(hints) || hints.root_note.is_some() {
        return (SampleKind::OneShot, None);
    }
    if let Some(bpm) = hints.bpm_guess
        && fits_tempo(dur, bpm, true)
    {
        return (SampleKind::Loop, Some(bpm));
    }
    let drum_bed = hints.category == Some(Category::Drums) && hints.subcategory.is_none();
    let rhythmic = onsets.is_some_and(|n| n >= MIN_LOOP_ONSETS);
    if (drum_bed || hints.loop_hint) && rhythmic && dur >= MIN_ANALYSED_LOOP_S {
        return (SampleKind::Loop, None);
    }
    (SampleKind::OneShot, None)
}

/// Counts attacks while a file is decoded, without allocating: the signal is
/// cut into 10 ms frames and an onset is a frame at least 9 dB louder than the
/// loudest of the previous three, above -45 dBFS, at least 70 ms after the
/// previous onset. Crude, but enough to tell one hit from a groove.
#[derive(Clone, Debug)]
pub struct OnsetCounter {
    hop: u64,
    filled: u64,
    acc: f64,
    prev: [f64; 3],
    since_last: u64,
    refractory: u64,
    pub count: u32,
}

impl OnsetCounter {
    const RISE: f64 = 7.94; // +9 dB in power
    const FLOOR: f64 = 3.16e-5; // -45 dBFS in power

    pub fn new(sample_rate: u32) -> Self {
        let hop = (sample_rate as u64 / 100).max(1);
        Self { hop, filled: 0, acc: 0.0, prev: [0.0; 3], since_last: u64::MAX / 2, refractory: 7, count: 0 }
    }

    /// Feeds one frame (mono mix of its channels).
    pub fn push(&mut self, v: f64) {
        self.acc += v * v;
        self.filled += 1;
        if self.filled < self.hop {
            return;
        }
        let e = self.acc / self.hop as f64;
        self.acc = 0.0;
        self.filled = 0;
        let before = self.prev.iter().copied().fold(0.0, f64::max);
        self.since_last = self.since_last.saturating_add(1);
        if e > Self::FLOOR && e > before.max(Self::FLOOR / 4.0) * Self::RISE && self.since_last >= self.refractory {
            self.count += 1;
            self.since_last = 0;
        }
        self.prev = [self.prev[1], self.prev[2], e];
    }
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
    let ch = spec.channels.max(1);
    let mut peak = 0f64;
    let mut sum_sq = 0f64;
    let mut n = 0u64;
    let mut onsets = OnsetCounter::new(spec.sample_rate);
    let mut mix = 0f64;
    let mut feed = |v: f64, n: u64| {
        mix += v;
        if (n + 1).is_multiple_of(ch as u64) {
            onsets.push(mix / ch as f64);
            mix = 0.0;
        }
    };
    match spec.sample_format {
        hound::SampleFormat::Float => {
            for s in r.samples::<f32>() {
                let v = s.map_err(|e| LibraryError::Audio(e.to_string()))? as f64;
                peak = peak.max(v.abs());
                sum_sq += v * v;
                feed(v, n);
                n += 1;
            }
        }
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample.clamp(1, 32) - 1)) as f64;
            for s in r.samples::<i32>() {
                let v = s.map_err(|e| LibraryError::Audio(e.to_string()))? as f64 * scale;
                peak = peak.max(v.abs());
                sum_sq += v * v;
                feed(v, n);
                n += 1;
            }
        }
    }
    let frames = n / ch as u64;
    Ok(AudioInfo {
        duration_s: frames as f64 / spec.sample_rate.max(1) as f64,
        sample_rate: spec.sample_rate,
        channels: ch,
        frames,
        peak_db: to_db(peak),
        rms_db: (n > 0).then(|| to_db((sum_sq / n as f64).sqrt())).flatten(),
        onsets: Some(onsets.count),
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
    let mut onsets: Option<OnsetCounter> = None;
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
        let oc = onsets.get_or_insert_with(|| OnsetCounter::new(spec.rate));
        let ch = (channels as usize).max(1);
        for frame in sb.samples().chunks(ch) {
            let mut mix = 0f64;
            for &s in frame {
                let v = s as f64;
                peak = peak.max(v.abs());
                sum_sq += v * v;
                mix += v;
                n += 1;
            }
            oc.push(mix / ch as f64);
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
        onsets: Some(onsets.map_or(0, |o| o.count)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::parse_path;
    use crate::test_util::write_wav;
    use std::path::Path;

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

    fn kind_of(name: &str, dur: f64, onsets: Option<u32>) -> (SampleKind, Option<f32>) {
        let a = analysis_from_hints(AudioInfo { duration_s: dur, onsets, ..Default::default() }, &parse_path(Path::new(name), None));
        (a.kind, a.bpm)
    }

    #[test]
    fn loop_or_one_shot_in_order_of_trust() {
        use SampleKind::{Loop, OneShot};
        // 1. The name says so, whatever the length.
        assert_eq!(kind_of("Synth_Am_128bpm_loop.wav", 8.0, Some(1)).0, Loop);
        assert_eq!(kind_of("Kick One Shot 128bpm.wav", 7.5, Some(9)).0, OneShot);
        assert_eq!(kind_of("Loops/Snare Hit.wav", 0.3, Some(1)).0, OneShot);
        // 2. A tempo in the name and a length of whole beats at it.
        assert_eq!(kind_of("Hats_128bpm.wav", 7.5, None), (Loop, Some(128.0)));
        assert_eq!(kind_of("Top_128bpm.wav", 7.47, None).0, Loop, "a trimmed export still fits");
        assert_eq!(kind_of("Kick_128bpm.wav", 0.3, None).0, OneShot);
        // 3. Drum hits and multisample notes are one-shots, however many attacks.
        assert_eq!(kind_of("Kick 1.wav", 0.4, Some(1)).0, OneShot);
        assert_eq!(kind_of("Snare_120.wav", 4.0, Some(8)).0, OneShot);
        assert_eq!(kind_of("Crash 2.wav", 6.7, Some(1)).0, OneShot);
        assert_eq!(kind_of("Samples/bass_8_e2.wav", 1.99, Some(1)), (OneShot, None));
        assert_eq!(kind_of("Samples/blackheart/A1.wav", 4.5, Some(5)).0, OneShot);
        // 4. A bare number that the length matches as whole bars.
        assert_eq!(kind_of("Perc_128.wav", 7.5, None), (Loop, Some(128.0)));
        assert_eq!(kind_of("Perc_128.wav", 2.2, None).0, OneShot);
        // 5. Audio: a drum bed or a "beat" with several attacks.
        let (k, bpm) = kind_of("arovane_beat_a.flac", 14.77, Some(32));
        assert_eq!(k, Loop);
        assert!(bpm.is_some());
        assert_eq!(kind_of("arovane_beat_a.flac", 14.77, Some(1)).0, OneShot);
        assert_eq!(kind_of("drum_roll.flac", 6.2, Some(30)).0, OneShot, "a roll is one gesture");
        // Sustained tonal material is never a loop by length alone.
        assert_eq!(kind_of("Pad_Warm.wav", 8.0, Some(1)).0, OneShot);
        assert_eq!(kind_of("a0_oscillator_tuned.wav", 1.93, Some(1)).0, OneShot);
        assert_eq!(kind_of("Untitled 3.wav", 8.0, Some(20)).0, OneShot);
    }

    #[test]
    fn onsets_are_counted_while_decoding() {
        use crate::test_util::write_clicks;
        let dir = tempfile::tempdir().unwrap();
        let clicks = dir.path().join("Drum Beat.wav");
        write_clicks(&clicks, 44_100, 120.0, 16);
        let a = BasicAnalyzer.analyze(&clicks, &parse_path(&clicks, Some(dir.path()))).unwrap();
        assert_eq!(a.audio.onsets, Some(16));
        assert_eq!(a.kind, SampleKind::Loop);
        assert_eq!(a.bpm, Some(120.0));

        let tone = dir.path().join("Pad Sine.wav");
        write_wav(&tone, 44_100, 2, 3.0, 0.5);
        let a = BasicAnalyzer.analyze(&tone, &parse_path(&tone, Some(dir.path()))).unwrap();
        assert_eq!(a.audio.onsets, Some(1));
        assert_eq!(a.kind, SampleKind::OneShot);
    }

    #[test]
    fn bpm_estimate() {
        assert_eq!(estimate_loop_bpm(2.0), Some(120.0));
        assert_eq!(estimate_loop_bpm(1.875), Some(128.0));
        assert_eq!(estimate_loop_bpm(0.01), None);
    }
}
