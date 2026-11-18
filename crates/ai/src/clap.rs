//! CLAP (LAION `larger_clap_general`, Apache-2.0) audio and text embeddings
//! for searching samples by meaning ("dark punchy kick", "warm analog pad").
//!
//! The two towers share one 512-value space: the audio tower turns a sound
//! into a vector, the text tower turns a phrase into one, and the cosine
//! between them says how well the phrase describes the sound. The model is an
//! ONNX export (int8, 205 MB in all) the [`ModelManager`] downloads on demand;
//! nothing here runs until it is installed.
//!
//! The model expects exactly what the Hugging Face `ClapFeatureExtractor`
//! produces, reproduced here without Python:
//!
//! * audio at 48 kHz, mono, cut or repeat-padded to 10 s (480 000 samples);
//! * a log-mel spectrogram: periodic Hann window of 1024, hop 480, centred
//!   frames with reflect padding (1001 frames), power spectrum, 64 Slaney mel
//!   bands from 50 Hz to 14 kHz with Slaney area normalisation, then
//!   `10·log10(max(x, 1e-10))`;
//! * text through the RoBERTa byte-level BPE (`vocab.json` + `merges.txt`),
//!   wrapped in `<s> … </s>`.
//!
//! Sounds longer than 10 s are listened to in up to [`MAX_WINDOWS`] evenly
//! spaced windows whose vectors are averaged. Everything but the ONNX
//! sessions is pure and unit-tested; the sessions sit behind [`ClapBackend`],
//! so the whole pipeline is tested with a fake model.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use realfft::{RealFftPlanner, RealToComplex};
use rustfft::num_complex::Complex32;

use crate::audio::{AudioData, load, resample_channel};
use crate::models::{ModelManager, ids};
use crate::progress::{CancelToken, Progress};
use crate::runtime::{self, Accel};
use crate::{Error, Result};

/// Stable name stored with each vector.
pub const CLAP_EMBEDDING_ID: &str = "clap-larger-general-int8";
/// Increase when the preprocessing or the model changes, so stored vectors are made again.
pub const CLAP_EMBEDDING_VERSION: u32 = 1;
/// Length of the vector.
pub const CLAP_EMBEDDING_DIM: usize = 512;

/// Sample rate the model listens at.
pub const CLAP_RATE: u32 = 48_000;
/// Samples of one window (10 s).
pub const WINDOW_SAMPLES: usize = 480_000;
/// Frames of the spectrogram of one window.
pub const MEL_FRAMES: usize = 1001;
/// Mel bands.
pub const MEL_BANDS: usize = 64;
/// Longest sound heard in full: more windows than this are not taken.
pub const MAX_WINDOWS: usize = 3;
/// Tokens of the longest phrase.
pub const MAX_TOKENS: usize = 77;

// ───────────────────────────── Log-mel features ─────────────────────────────

/// Parameters of a log-mel front end.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MelSpec {
    pub sample_rate: u32,
    pub n_fft: usize,
    pub hop: usize,
    pub n_mel: usize,
    pub f_min: f32,
    pub f_max: f32,
}

impl MelSpec {
    /// The front end of `larger_clap_general`.
    pub const CLAP: Self = Self { sample_rate: CLAP_RATE, n_fft: 1024, hop: 480, n_mel: MEL_BANDS, f_min: 50.0, f_max: 14_000.0 };
}

fn hz_to_mel(hz: f64) -> f64 {
    // Slaney: linear up to 1 kHz (200/3 Hz per mel), logarithmic above.
    const MIN_LOG_HZ: f64 = 1000.0;
    const MIN_LOG_MEL: f64 = 15.0;
    let logstep = 6.4f64.ln() / 27.0;
    if hz >= MIN_LOG_HZ { MIN_LOG_MEL + (hz / MIN_LOG_HZ).ln() / logstep } else { hz / (200.0 / 3.0) }
}

fn mel_to_hz(mel: f64) -> f64 {
    const MIN_LOG_HZ: f64 = 1000.0;
    const MIN_LOG_MEL: f64 = 15.0;
    let logstep = 6.4f64.ln() / 27.0;
    if mel >= MIN_LOG_MEL { MIN_LOG_HZ * (logstep * (mel - MIN_LOG_MEL)).exp() } else { mel * (200.0 / 3.0) }
}

/// Slaney-scale, Slaney-normalised triangular filters: `n_mel` rows of `n_fft / 2 + 1` weights.
pub fn mel_filter_bank(spec: &MelSpec) -> Vec<Vec<f32>> {
    let bins = spec.n_fft / 2 + 1;
    let (lo, hi) = (hz_to_mel(spec.f_min as f64), hz_to_mel(spec.f_max as f64));
    let edges: Vec<f64> = (0..spec.n_mel + 2).map(|i| mel_to_hz(lo + (hi - lo) * i as f64 / (spec.n_mel + 1) as f64)).collect();
    let fft_freqs: Vec<f64> = (0..bins).map(|i| (spec.sample_rate / 2) as f64 * i as f64 / (bins - 1) as f64).collect();
    (0..spec.n_mel)
        .map(|m| {
            let (l, c, r) = (edges[m], edges[m + 1], edges[m + 2]);
            let norm = 2.0 / (r - l);
            fft_freqs
                .iter()
                .map(|&f| {
                    let down = (f - l) / (c - l);
                    let up = (r - f) / (r - c);
                    (down.min(up).max(0.0) * norm) as f32
                })
                .collect()
        })
        .collect()
}

/// Turns 10 s of 48 kHz audio into the model's input. Holds the filter bank, the window and the FFT plan.
pub struct MelExtractor {
    spec: MelSpec,
    filters: Vec<Vec<f32>>,
    window: Vec<f32>,
    fft: Arc<dyn RealToComplex<f32>>,
}

impl MelExtractor {
    pub fn new(spec: MelSpec) -> Self {
        // Periodic Hann, like `scipy.signal.get_window` and the Hugging Face extractor.
        let window = (0..spec.n_fft).map(|i| 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / spec.n_fft as f32).cos()).collect();
        Self { spec, filters: mel_filter_bank(&spec), window, fft: RealFftPlanner::<f32>::new().plan_fft_forward(spec.n_fft) }
    }

    pub fn clap() -> Self {
        Self::new(MelSpec::CLAP)
    }

    pub fn spec(&self) -> &MelSpec {
        &self.spec
    }

    /// Frames a signal of `len` samples gives (centred, so `1 + len / hop`).
    pub fn frames_for(&self, len: usize) -> usize {
        1 + len / self.spec.hop
    }

    /// Log-mel power in dB, frame-major: `frames × n_mel` values.
    pub fn log_mel(&self, x: &[f32]) -> Vec<f32> {
        let (n_fft, hop, n_mel) = (self.spec.n_fft, self.spec.hop, self.spec.n_mel);
        let half = n_fft / 2;
        // Reflect padding (numpy `reflect`: the edge sample is not repeated).
        let reflect = |i: isize| -> f32 {
            let n = x.len() as isize;
            if n == 0 {
                return 0.0;
            }
            if n == 1 {
                return x[0];
            }
            let period = 2 * (n - 1);
            let mut j = i.rem_euclid(period);
            if j >= n {
                j = period - j;
            }
            x[j as usize]
        };
        let frames = self.frames_for(x.len());
        let mut out = Vec::with_capacity(frames * n_mel);
        let mut input = self.fft.make_input_vec();
        let mut spectrum: Vec<Complex32> = self.fft.make_output_vec();
        let mut scratch = self.fft.make_scratch_vec();
        let mut power = vec![0f32; n_fft / 2 + 1];
        for f in 0..frames {
            let start = (f * hop) as isize - half as isize;
            for (i, v) in input.iter_mut().enumerate() {
                *v = reflect(start + i as isize) * self.window[i];
            }
            // The input length always matches the plan; the call cannot fail.
            let _ = self.fft.process_with_scratch(&mut input, &mut spectrum, &mut scratch);
            for (p, c) in power.iter_mut().zip(&spectrum) {
                *p = c.re * c.re + c.im * c.im;
            }
            for row in &self.filters {
                let mel: f32 = row.iter().zip(&power).map(|(w, p)| w * p).sum();
                out.push(10.0 * mel.max(1e-10).log10());
            }
        }
        out
    }
}

/// Exactly `len` samples the way `ClapFeatureExtractor` does with `repeatpad`:
/// a short sound is repeated as many whole times as fit and padded with zeros;
/// a long one is cut to its first `len` samples.
pub fn repeat_pad(x: &[f32], len: usize) -> Vec<f32> {
    if x.is_empty() {
        return vec![0.0; len];
    }
    if x.len() >= len {
        return x[..len].to_vec();
    }
    let mut out = Vec::with_capacity(len);
    for _ in 0..len / x.len() {
        out.extend_from_slice(x);
    }
    out.resize(len, 0.0);
    out
}

/// Start offsets of the windows a sound of `len` samples is heard in: one for
/// a sound that fits, else up to `max` evenly spaced ones covering the start and the end.
pub fn window_starts(len: usize, win: usize, max: usize) -> Vec<usize> {
    if len <= win || max <= 1 {
        return vec![0];
    }
    let n = len.div_ceil(win).min(max);
    (0..n).map(|i| i * (len - win) / (n - 1).max(1)).collect()
}

/// L2-normalises `v` in place (a zero vector stays zero).
pub fn normalise(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 1e-12 {
        v.iter_mut().for_each(|x| *x /= norm);
    }
}

/// Mean of the unit-length versions of `vectors`, itself of length 1.
pub fn pool(vectors: &[Vec<f32>]) -> Vec<f32> {
    let Some(first) = vectors.first() else { return Vec::new() };
    let mut sum = vec![0f32; first.len()];
    for v in vectors {
        let mut u = v.clone();
        normalise(&mut u);
        sum.iter_mut().zip(&u).for_each(|(s, x)| *s += x);
    }
    normalise(&mut sum);
    sum
}

// ───────────────────────────── Tokenizer ─────────────────────────────

/// RoBERTa's byte-level BPE, driven by the model's `vocab.json` and `merges.txt`.
pub struct Tokenizer {
    vocab: HashMap<String, u32>,
    ranks: HashMap<(String, String), usize>,
    byte_char: [char; 256],
    bos: u32,
    eos: u32,
    unk: u32,
}

impl Tokenizer {
    pub fn from_files(vocab_json: &Path, merges_txt: &Path) -> Result<Self> {
        let vocab = std::fs::read_to_string(vocab_json).map_err(|e| Error::io(vocab_json, e))?;
        let merges = std::fs::read_to_string(merges_txt).map_err(|e| Error::io(merges_txt, e))?;
        Self::from_text(&vocab, &merges)
    }

    pub fn from_text(vocab_json: &str, merges_txt: &str) -> Result<Self> {
        let vocab: HashMap<String, u32> = serde_json::from_str(vocab_json).map_err(|e| Error::InvalidInput(format!("vocab.json: {e}")))?;
        let ranks: HashMap<(String, String), usize> = merges_txt
            .lines()
            .filter(|l| !l.starts_with("#version") && !l.trim().is_empty())
            .enumerate()
            .filter_map(|(i, l)| l.split_once(' ').map(|(a, b)| ((a.to_owned(), b.to_owned()), i)))
            .collect();
        let special = |t: &str, fallback: u32| vocab.get(t).copied().unwrap_or(fallback);
        let (bos, eos, unk) = (special("<s>", 0), special("</s>", 2), special("<unk>", 3));
        Ok(Self { vocab, ranks, byte_char: byte_chars(), bos, eos, unk })
    }

    /// `<s> … </s>` for `text`, at most `max` tokens (the end token is always kept).
    pub fn encode(&self, text: &str, max: usize) -> Vec<i64> {
        let mut ids = vec![self.bos as i64];
        'words: for piece in pre_tokenize(text) {
            let mapped: String = piece.bytes().map(|b| self.byte_char[b as usize]).collect();
            for tok in self.bpe(&mapped) {
                if ids.len() + 1 >= max.max(2) {
                    break 'words;
                }
                ids.push(self.vocab.get(&tok).copied().unwrap_or(self.unk) as i64);
            }
        }
        ids.push(self.eos as i64);
        ids
    }

    /// Merges the characters of one pre-token until no rule applies.
    fn bpe(&self, word: &str) -> Vec<String> {
        let mut parts: Vec<String> = word.chars().map(String::from).collect();
        while parts.len() > 1 {
            let best = parts.windows(2).enumerate().filter_map(|(i, w)| self.ranks.get(&(w[0].clone(), w[1].clone())).map(|r| (*r, i))).min();
            let Some((_, i)) = best else { break };
            let merged = format!("{}{}", parts[i], parts[i + 1]);
            parts.splice(i..=i + 1, [merged]);
        }
        parts
    }
}

/// GPT-2's reversible byte → printable character map.
fn byte_chars() -> [char; 256] {
    let mut out = ['\0'; 256];
    let mut n = 0u32;
    for b in 0..=255u32 {
        let keep = (0x21..=0x7e).contains(&b) || (0xa1..=0xac).contains(&b) || (0xae..=0xff).contains(&b);
        out[b as usize] = if keep {
            char::from_u32(b).unwrap_or('?')
        } else {
            n += 1;
            char::from_u32(255 + n).unwrap_or('?')
        };
    }
    out
}

/// GPT-2's pre-tokenisation (`'s|'t|'re|'ve|'m|'ll|'d| ?letters| ?digits| ?other|\s+(?!\S)|\s+`) by hand.
pub fn pre_tokenize(text: &str) -> Vec<&str> {
    #[derive(PartialEq, Clone, Copy)]
    enum Class {
        Letter,
        Digit,
        Space,
        Other,
    }
    let class = |c: char| {
        if c.is_alphabetic() {
            Class::Letter
        } else if c.is_numeric() {
            Class::Digit
        } else if c.is_whitespace() {
            Class::Space
        } else {
            Class::Other
        }
    };
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let n = chars.len();
    let at = |i: usize| chars.get(i).map_or(text.len(), |c| c.0);
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        let c = chars[i].1;
        if c == '\'' {
            let rest = &text[at(i)..];
            if let Some(len) = ["'s", "'t", "'re", "'ve", "'m", "'ll", "'d"].iter().find(|s| rest.starts_with(**s)).map(|s| s.len()) {
                out.push(&rest[..len]);
                i += len;
                continue;
            }
        }
        // A word, a number or a run of symbols, with the one plain space before it.
        let j = if c == ' ' { i + 1 } else { i };
        if j < n && class(chars[j].1) != Class::Space {
            let cls = class(chars[j].1);
            let mut k = j + 1;
            while k < n && class(chars[k].1) == cls {
                k += 1;
            }
            out.push(&text[at(i)..at(k)]);
            i = k;
            continue;
        }
        // A run of white space: the last one stays for the next word.
        let mut k = i;
        while k < n && class(chars[k].1) == Class::Space {
            k += 1;
        }
        let end = if k == n || k - i == 1 { k } else { k - 1 };
        out.push(&text[at(i)..at(end)]);
        i = end;
    }
    out
}

// ───────────────────────────── The model ─────────────────────────────

/// The two towers. [`OrtBackend`] runs the ONNX files; tests use a fake.
pub trait ClapBackend: Send + Sync {
    /// Vector length.
    fn dim(&self) -> usize;
    /// One window of log-mel features (`MEL_FRAMES × MEL_BANDS`, frame-major) to its embedding.
    fn embed_features(&self, features: &[f32]) -> Result<Vec<f32>>;
    /// One phrase of token ids (`<s> … </s>`) to its embedding.
    fn embed_ids(&self, ids: &[i64]) -> Result<Vec<f32>>;
}

/// Audio and text embeddings: the pipeline around a [`ClapBackend`].
pub struct Clap {
    backend: Box<dyn ClapBackend>,
    mel: MelExtractor,
    tokenizer: Tokenizer,
}

impl Clap {
    pub fn new(backend: Box<dyn ClapBackend>, tokenizer: Tokenizer) -> Self {
        Self { backend, mel: MelExtractor::clap(), tokenizer }
    }

    pub fn dim(&self) -> usize {
        self.backend.dim()
    }

    /// Decodes `path` and embeds it.
    pub fn embed_file(&self, path: impl AsRef<Path>) -> Result<Vec<f32>> {
        let path = path.as_ref();
        let audio = load(path)?;
        self.embed_audio(&audio).map_err(|e| match e {
            Error::InvalidInput(m) => Error::InvalidInput(format!("{}: {m}", path.display())),
            e => e,
        })
    }

    /// The unit-length embedding of decoded audio; an error for silence.
    pub fn embed_audio(&self, audio: &AudioData) -> Result<Vec<f32>> {
        let mono = audio.to_mono();
        if mono.len() < 64 {
            return Err(Error::InvalidInput("too short to describe".into()));
        }
        let x = resample_channel(&mono, audio.sample_rate, CLAP_RATE)?;
        let peak = x.iter().fold(0f32, |m, v| m.max(v.abs()));
        if peak < 1e-6 || !peak.is_finite() {
            return Err(Error::InvalidInput("silent".into()));
        }
        let mut vectors = Vec::new();
        for start in window_starts(x.len(), WINDOW_SAMPLES, MAX_WINDOWS) {
            let end = (start + WINDOW_SAMPLES).min(x.len());
            let window = repeat_pad(&x[start..end], WINDOW_SAMPLES);
            let features = self.mel.log_mel(&window);
            vectors.push(self.backend.embed_features(&features)?);
        }
        Ok(pool(&vectors))
    }

    /// The unit-length embedding of a phrase.
    pub fn embed_text(&self, text: &str) -> Result<Vec<f32>> {
        let text = text.trim();
        if text.is_empty() {
            return Err(Error::InvalidInput("empty text".into()));
        }
        let ids = self.tokenizer.encode(text, MAX_TOKENS);
        let mut v = self.backend.embed_ids(&ids)?;
        normalise(&mut v);
        Ok(v)
    }

    /// Opens the installed model. Fails with [`Error::NotInstalled`] when the model or ONNX Runtime
    /// is missing: nothing is downloaded here (see [`install`]).
    pub fn open(manager: &ModelManager) -> Result<Self> {
        let model = manager.installed(ids::CLAP_GENERAL).ok_or_else(|| Error::NotInstalled(ids::CLAP_GENERAL.into()))?;
        if !runtime_ready(manager) {
            return Err(Error::NotInstalled(ids::ONNXRUNTIME.into()));
        }
        Self::open_dir(manager, &model.dir)
    }

    /// Opens the model files found in `dir` (the layout of the manifest entry).
    pub fn open_dir(manager: &ModelManager, dir: &Path) -> Result<Self> {
        let entry = manager.entry(ids::CLAP_GENERAL)?;
        let names = entry.installed_names();
        let path = |i: usize| dir.join(&names[i]);
        let tokenizer = Tokenizer::from_files(&path(2), &path(3))?;
        let backend = OrtBackend::open(manager, &path(0), &path(1))?;
        Ok(Self::new(Box::new(backend), tokenizer))
    }
}

/// Whether ONNX Runtime can be loaded without downloading it.
pub fn runtime_ready(manager: &ModelManager) -> bool {
    runtime::loaded().is_some()
        || std::env::var_os("AITRACK_ORT_DYLIB").is_some()
        || manager.runtime_id(false).ok().is_some_and(|id| manager.installed(id).is_some())
}

/// Whether the CLAP files are installed (and the runtime to run them).
pub fn is_ready(manager: &ModelManager) -> bool {
    manager.installed(ids::CLAP_GENERAL).is_some() && runtime_ready(manager)
}

/// Downloads the model and, if needed, ONNX Runtime. `progress` runs on the calling thread.
pub fn install(manager: &ModelManager, progress: &mut dyn FnMut(Progress), cancel: &CancelToken) -> Result<()> {
    if !runtime_ready(manager) {
        let id = manager.runtime_id(false)?;
        manager.ensure_cancellable(id, &mut |p| progress(Progress { fraction: p.fraction * 0.15, ..p }), cancel)?;
    }
    manager.ensure_cancellable(ids::CLAP_GENERAL, &mut |p| progress(Progress { fraction: 0.15 + p.fraction * 0.85, ..p }), cancel)?;
    Ok(())
}

/// The ONNX sessions of the audio and text towers.
pub struct OrtBackend {
    audio: Mutex<ort::session::Session>,
    text: Mutex<ort::session::Session>,
    audio_in: String,
    audio_longer: Option<String>,
    audio_out: String,
    ids_in: String,
    mask_in: Option<String>,
    text_out: String,
    dim: usize,
}

fn pick_output(session: &ort::session::Session, wanted: &str) -> Result<String> {
    let outputs = session.outputs();
    outputs
        .iter()
        .find(|o| o.name().contains(wanted))
        .or(outputs.first())
        .map(|o| o.name().to_string())
        .ok_or_else(|| Error::ModelOutput("the model has no outputs".into()))
}

impl OrtBackend {
    pub fn open(manager: &ModelManager, audio: &Path, text: &Path) -> Result<Self> {
        // CPU: a 10 s window is one small batch, and the CoreML/DirectML providers
        // compile slowly and handle int8 operators badly.
        let audio = runtime::session(manager, audio, Accel::Cpu, &mut |_| {})?;
        let text = runtime::session(manager, text, Accel::Cpu, &mut |_| {})?;
        let find = |s: &ort::session::Session, key: &str| s.inputs().iter().map(|i| i.name().to_string()).find(|n| n.contains(key));
        let audio_in = find(&audio, "feature").or_else(|| audio.inputs().first().map(|i| i.name().to_string())).ok_or_else(|| Error::ModelOutput("the audio model has no inputs".into()))?;
        let audio_longer = find(&audio, "longer");
        let ids_in = find(&text, "ids").or_else(|| text.inputs().first().map(|i| i.name().to_string())).ok_or_else(|| Error::ModelOutput("the text model has no inputs".into()))?;
        let mask_in = find(&text, "mask");
        let audio_out = pick_output(&audio, "embeds")?;
        let text_out = pick_output(&text, "embeds")?;
        Ok(Self {
            dim: CLAP_EMBEDDING_DIM,
            audio: Mutex::new(audio),
            text: Mutex::new(text),
            audio_in,
            audio_longer,
            audio_out,
            ids_in,
            mask_in,
            text_out,
        })
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl ClapBackend for OrtBackend {
    fn dim(&self) -> usize {
        self.dim
    }

    fn embed_features(&self, features: &[f32]) -> Result<Vec<f32>> {
        let t = ort::value::Tensor::from_array(([1usize, 1, MEL_FRAMES, MEL_BANDS], features.to_vec()))?;
        let mut session = lock(&self.audio);
        let out = match &self.audio_longer {
            Some(longer) => {
                let l = ort::value::Tensor::from_array(([1usize, 1], vec![false]))?;
                session.run(ort::inputs![self.audio_in.as_str() => t, longer.as_str() => l])?
            }
            None => session.run(ort::inputs![self.audio_in.as_str() => t])?,
        };
        let v = out.get(self.audio_out.as_str()).ok_or_else(|| Error::ModelOutput(format!("missing output {}", self.audio_out)))?;
        let (_, data) = v.try_extract_tensor::<f32>()?;
        Ok(data.to_vec())
    }

    fn embed_ids(&self, ids: &[i64]) -> Result<Vec<f32>> {
        let n = ids.len();
        let t = ort::value::Tensor::from_array(([1usize, n], ids.to_vec()))?;
        let mut session = lock(&self.text);
        let out = match &self.mask_in {
            Some(mask) => {
                let m = ort::value::Tensor::from_array(([1usize, n], vec![1i64; n]))?;
                session.run(ort::inputs![self.ids_in.as_str() => t, mask.as_str() => m])?
            }
            None => session.run(ort::inputs![self.ids_in.as_str() => t])?,
        };
        let v = out.get(self.text_out.as_str()).ok_or_else(|| Error::ModelOutput(format!("missing output {}", self.text_out)))?;
        let (_, data) = v.try_extract_tensor::<f32>()?;
        Ok(data.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reference STFT by the definition, no FFT: power of each centred, windowed frame in dB per mel band.
    fn naive_log_mel(x: &[f32], spec: &MelSpec) -> Vec<f32> {
        let filters = mel_filter_bank(spec);
        let n = x.len() as isize;
        let at = |i: isize| -> f64 {
            let period = 2 * (n - 1);
            let mut j = i.rem_euclid(period);
            if j >= n {
                j = period - j;
            }
            x[j as usize] as f64
        };
        let frames = 1 + x.len() / spec.hop;
        let mut out = Vec::new();
        for f in 0..frames {
            let start = (f * spec.hop) as isize - (spec.n_fft / 2) as isize;
            let frame: Vec<f64> = (0..spec.n_fft)
                .map(|i| at(start + i as isize) * (0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / spec.n_fft as f64).cos()))
                .collect();
            let power: Vec<f64> = (0..=spec.n_fft / 2)
                .map(|k| {
                    let (mut re, mut im) = (0.0, 0.0);
                    for (i, v) in frame.iter().enumerate() {
                        let a = -2.0 * std::f64::consts::PI * (k * i) as f64 / spec.n_fft as f64;
                        re += v * a.cos();
                        im += v * a.sin();
                    }
                    re * re + im * im
                })
                .collect();
            for row in &filters {
                let mel: f64 = row.iter().zip(&power).map(|(w, p)| *w as f64 * p).sum();
                out.push((10.0 * mel.max(1e-10).log10()) as f32);
            }
        }
        out
    }

    #[test]
    fn slaney_scale_matches_known_points() {
        assert!((hz_to_mel(1000.0) - 15.0).abs() < 1e-9);
        assert!((hz_to_mel(200.0) - 3.0).abs() < 1e-9);
        // 10 kHz: 15 + ln(10) / (ln(6.4) / 27) = 48.4913...
        assert!((hz_to_mel(10_000.0) - 48.4913).abs() < 1e-3, "{}", hz_to_mel(10_000.0));
        for hz in [50.0, 440.0, 999.0, 1000.0, 4321.0, 14_000.0] {
            assert!((mel_to_hz(hz_to_mel(hz)) - hz).abs() < 1e-6 * hz.max(1.0));
        }
    }

    #[test]
    fn filter_bank_is_triangular_and_slaney_normalised() {
        let spec = MelSpec::CLAP;
        let fb = mel_filter_bank(&spec);
        assert_eq!((fb.len(), fb[0].len()), (64, 513));
        let bin_hz = 24_000.0 / 512.0;
        for (m, row) in fb.iter().enumerate() {
            assert!(row.iter().all(|w| *w >= 0.0));
            // Slaney normalisation: each filter integrates to about 1 over frequency.
            let area: f32 = row.iter().sum::<f32>() * bin_hz;
            assert!((0.8..1.25).contains(&area), "band {m}: {area}");
            // One peak, nothing outside f_min..f_max.
            let nonzero: Vec<usize> = row.iter().enumerate().filter(|(_, w)| **w > 0.0).map(|(i, _)| i).collect();
            assert!(nonzero.first().is_some_and(|&i| i as f32 * bin_hz >= spec.f_min - bin_hz));
            assert!(nonzero.last().is_some_and(|&i| i as f32 * bin_hz <= spec.f_max + bin_hz));
        }
        // The bands climb in frequency.
        let peak = |row: &Vec<f32>| row.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap().0;
        assert!(fb.windows(2).all(|w| peak(&w[0]) <= peak(&w[1])));
    }

    #[test]
    fn log_mel_matches_a_naive_dft_reference() {
        let spec = MelSpec { sample_rate: 8000, n_fft: 32, hop: 8, n_mel: 6, f_min: 100.0, f_max: 3800.0 };
        let x: Vec<f32> = (0..100).map(|i| ((i as f32 * 0.37).sin() * 0.6 + (i as f32 * 1.9).cos() * 0.3) * (1.0 - i as f32 / 140.0)).collect();
        let got = MelExtractor::new(spec).log_mel(&x);
        let want = naive_log_mel(&x, &spec);
        assert_eq!(got.len(), want.len());
        assert_eq!(got.len(), (1 + 100 / 8) * 6);
        for (i, (g, w)) in got.iter().zip(&want).enumerate() {
            assert!((g - w).abs() < 0.02, "value {i}: {g} vs {w}");
        }
    }

    #[test]
    fn a_clap_window_has_1001_frames_and_a_tone_lights_its_band() {
        let ex = MelExtractor::clap();
        let x: Vec<f32> = (0..WINDOW_SAMPLES).map(|i| (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / CLAP_RATE as f32).sin() * 0.5).collect();
        let m = ex.log_mel(&x);
        assert_eq!(m.len(), MEL_FRAMES * MEL_BANDS);
        let frame = &m[500 * MEL_BANDS..501 * MEL_BANDS];
        let loudest = frame.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap().0;
        // 1 kHz is mel 15; the bands span mel 0.75..48.9 in 64 steps of 0.77: band ~ 18.
        assert!((16..=20).contains(&loudest), "{loudest}");
        // Silence sits at the floor.
        let silent = ex.log_mel(&vec![0.0; WINDOW_SAMPLES]);
        assert!(silent.iter().all(|v| (*v + 100.0).abs() < 1e-3));
    }

    #[test]
    fn repeat_pad_repeats_whole_copies_then_zeros() {
        assert_eq!(repeat_pad(&[1.0, 2.0, 3.0], 8), vec![1.0, 2.0, 3.0, 1.0, 2.0, 3.0, 0.0, 0.0]);
        assert_eq!(repeat_pad(&[1.0, 2.0, 3.0], 3), vec![1.0, 2.0, 3.0]);
        assert_eq!(repeat_pad(&[1.0, 2.0, 3.0, 4.0], 2), vec![1.0, 2.0]);
        assert_eq!(repeat_pad(&[], 3), vec![0.0; 3]);
    }

    #[test]
    fn long_sounds_are_heard_in_spread_windows() {
        assert_eq!(window_starts(100, 480, 3), vec![0]);
        assert_eq!(window_starts(480, 480, 3), vec![0]);
        assert_eq!(window_starts(600, 480, 3), vec![0, 120]);
        assert_eq!(window_starts(2000, 480, 3), vec![0, 760, 1520]);
        assert_eq!(window_starts(100_000, 480, 3).len(), 3);
        assert_eq!(*window_starts(100_000, 480, 3).last().unwrap(), 100_000 - 480, "the last window ends with the sound");
    }

    #[test]
    fn pooling_averages_unit_vectors() {
        let p = pool(&[vec![3.0, 0.0], vec![0.0, 100.0]]);
        assert!((p[0] - p[1]).abs() < 1e-6, "loudness of a window does not weigh: {p:?}");
        assert!((p.iter().map(|x| x * x).sum::<f32>() - 1.0).abs() < 1e-5);
        assert!(pool(&[]).is_empty());
        let mut z = vec![0.0, 0.0];
        normalise(&mut z);
        assert_eq!(z, vec![0.0, 0.0]);
    }

    fn tiny_tokenizer() -> Tokenizer {
        // The byte alphabet mapped as GPT-2 does, a few merges, and the specials.
        let chars = byte_chars();
        let mut vocab: Vec<String> = vec!["<s>".into(), "<pad>".into(), "</s>".into(), "<unk>".into()];
        for c in chars {
            vocab.push(c.to_string());
        }
        for extra in ["ki", "kic", "kick", "Ġk", "Ġkick", "da", "dark", "Ġd", "Ġdark"] {
            vocab.push(extra.to_owned());
        }
        let map: serde_json::Map<String, serde_json::Value> = vocab.iter().enumerate().map(|(i, t)| (t.clone(), serde_json::json!(i))).collect();
        let merges = "#version: 0.2\nk i\nki c\nkic k\nĠ kick\nd a\nda r\ndar k\nĠ dark\n";
        Tokenizer::from_text(&serde_json::Value::Object(map).to_string(), merges).unwrap()
    }

    #[test]
    fn byte_map_is_the_gpt2_one() {
        let c = byte_chars();
        assert_eq!(c[b'a' as usize], 'a');
        assert_eq!(c[b' ' as usize], 'Ġ');
        assert_eq!(c[b'\n' as usize], 'Ċ');
        assert_eq!(c[0], 'Ā');
        let distinct: std::collections::HashSet<char> = c.iter().copied().collect();
        assert_eq!(distinct.len(), 256);
    }

    #[test]
    fn pre_tokenizing_follows_the_gpt2_pattern() {
        assert_eq!(pre_tokenize("dark punchy kick"), vec!["dark", " punchy", " kick"]);
        assert_eq!(pre_tokenize("it's 808, ok!"), vec!["it", "'s", " 808", ",", " ok", "!"]);
        assert_eq!(pre_tokenize("a  b"), vec!["a", " ", " b"]);
        assert_eq!(pre_tokenize("end  "), vec!["end", "  "]);
        assert_eq!(pre_tokenize("Hi-hat"), vec!["Hi", "-", "hat"]);
        assert_eq!(pre_tokenize("café ñ"), vec!["café", " ñ"]);
        assert!(pre_tokenize("").is_empty());
    }

    #[test]
    fn the_tokenizer_applies_merges_in_rank_order() {
        let t = tiny_tokenizer();
        let id = |s: &str| t.vocab[s] as i64;
        assert_eq!(t.encode("kick", 77), vec![0, id("kick"), 2]);
        assert_eq!(t.encode("dark kick", 77), vec![0, id("dark"), id("Ġkick"), 2]);
        // No rule for "kicks": the rest stays single characters.
        assert_eq!(t.encode("kicks", 77), vec![0, id("kick"), id("s"), 2]);
        // A letter the vocabulary lacks is <unk>.
        let t2 = Tokenizer::from_text(r#"{"<s>":0,"<pad>":1,"</s>":2,"<unk>":3,"a":4}"#, "#version: 0.2\n").unwrap();
        assert_eq!(t2.encode("ab", 77), vec![0, 4, 3, 2]);
    }

    #[test]
    fn long_phrases_are_cut_but_keep_their_end() {
        let t = tiny_tokenizer();
        let ids = t.encode(&"kick ".repeat(200), 10);
        assert_eq!(ids.len(), 10);
        assert_eq!((ids[0], ids[9]), (0, 2));
    }

    /// Fake towers: the audio vector is the mean mel per eight bands (so a bright sound and a dark
    /// one differ); the text vector counts words from two small lists.
    struct Fake;

    impl ClapBackend for Fake {
        fn dim(&self) -> usize {
            8
        }
        fn embed_features(&self, f: &[f32]) -> Result<Vec<f32>> {
            assert_eq!(f.len(), MEL_FRAMES * MEL_BANDS);
            let mut v = vec![0f32; 8];
            for frame in f.chunks_exact(MEL_BANDS) {
                for (b, x) in frame.iter().enumerate() {
                    v[b / 8] += (x + 100.0) / MEL_FRAMES as f32;
                }
            }
            Ok(v)
        }
        fn embed_ids(&self, ids: &[i64]) -> Result<Vec<f32>> {
            let mut v = vec![0f32; 8];
            for id in ids {
                v[(*id as usize) % 8] += 1.0;
            }
            Ok(v)
        }
    }

    fn fake_clap() -> Clap {
        Clap::new(Box::new(Fake), tiny_tokenizer())
    }

    fn tone(hz: f32, seconds: f32, rate: u32) -> AudioData {
        let n = (seconds * rate as f32) as usize;
        AudioData::mono_signal((0..n).map(|i| (2.0 * std::f32::consts::PI * hz * i as f32 / rate as f32).sin() * 0.5).collect(), rate)
    }

    #[test]
    fn the_pipeline_embeds_audio_and_text_with_a_fake_model() {
        let clap = fake_clap();
        // Different input rates reach the same 48 kHz window.
        let low = clap.embed_audio(&tone(150.0, 1.0, 44_100)).unwrap();
        let low48 = clap.embed_audio(&tone(150.0, 1.0, 48_000)).unwrap();
        let high = clap.embed_audio(&tone(9000.0, 1.0, 44_100)).unwrap();
        let cos = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
        assert_eq!(low.len(), 8);
        assert!((cos(&low, &low) - 1.0).abs() < 1e-4);
        assert!(cos(&low, &low48) > 0.999, "{}", cos(&low, &low48));
        assert!(cos(&low, &low48) > cos(&low, &high));
        let t = clap.embed_text("  dark kick ").unwrap();
        assert_eq!(t.len(), 8);
        assert!((t.iter().map(|x| x * x).sum::<f32>() - 1.0).abs() < 1e-5);
        assert_eq!(t, clap.embed_text("dark kick").unwrap(), "surrounding spaces do not matter");
        assert!(clap.embed_text("   ").is_err());
    }

    #[test]
    fn silence_and_scraps_are_not_embedded_and_long_audio_is_pooled() {
        let clap = fake_clap();
        assert!(clap.embed_audio(&AudioData::mono_signal(vec![0.0; 48_000], 48_000)).is_err());
        assert!(clap.embed_audio(&AudioData::mono_signal(vec![0.5; 10], 48_000)).is_err());
        // 25 s: three windows, still one unit vector.
        let long = clap.embed_audio(&tone(300.0, 25.0, 48_000)).unwrap();
        assert!((long.iter().map(|x| x * x).sum::<f32>() - 1.0).abs() < 1e-4);
    }

    // ---- Real model (needs the files and a network or AITRACK_CLAP_DIR) ----

    fn synth_kick(rate: u32) -> AudioData {
        let n = (0.6 * rate as f32) as usize;
        let mut phase = 0f32;
        let x = (0..n)
            .map(|i| {
                let t = i as f32 / rate as f32;
                phase += 2.0 * std::f32::consts::PI * (45.0 + 110.0 * (-t * 28.0).exp()) / rate as f32;
                phase.sin() * (-t * 7.0).exp() * 0.9
            })
            .collect();
        AudioData::mono_signal(x, rate)
    }

    fn synth_hat(rate: u32) -> AudioData {
        let n = (0.18 * rate as f32) as usize;
        let (mut seed, mut prev) = (12345u32, 0f32);
        let x = (0..n)
            .map(|i| {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let white = (seed >> 8) as f32 / (1u32 << 23) as f32 - 1.0;
                // First difference: tilts the noise towards the highs.
                let hp = white - prev;
                prev = white;
                hp * 0.5 * (-(i as f32 / rate as f32) * 45.0).exp()
            })
            .collect();
        AudioData::mono_signal(x, rate)
    }

    fn synth_pad(rate: u32) -> AudioData {
        let n = (3.0 * rate as f32) as usize;
        let x = (0..n)
            .map(|i| {
                let t = i as f32 / rate as f32;
                let env = (t / 0.8).min(1.0) * (1.0 - (t - 2.2).max(0.0) / 0.8).max(0.0);
                let saw = |f: f32| 2.0 * ((t * f) % 1.0) - 1.0;
                let chord: f32 = [110.0, 138.6, 164.8, 220.5, 110.4].iter().map(|f| saw(*f)).sum::<f32>() / 5.0;
                chord * env * 0.5
            })
            .collect();
        AudioData::mono_signal(x, rate)
    }

    /// Runs the real int8 model on synthetic kick, hat and pad. Needs `AITRACK_MODELS_DIR` with the model
    /// installed (or a network: `install` downloads it, and ONNX Runtime, there).
    #[test]
    #[ignore = "needs the CLAP model (about 205 MB) and ONNX Runtime; downloads them into AITRACK_MODELS_DIR if missing"]
    fn the_real_model_ranks_a_kick_above_a_hat_for_kick_and_the_reverse_for_hat() {
        let manager = ModelManager::new().unwrap();
        install(&manager, &mut |p| eprintln!("{:.0}% {}", p.fraction * 100.0, p.message), &CancelToken::new()).unwrap();
        let clap = Clap::open(&manager).unwrap();
        let sounds = [("kick", synth_kick(44_100)), ("hat", synth_hat(44_100)), ("pad", synth_pad(44_100))];
        let vectors: Vec<(&str, Vec<f32>)> = sounds.iter().map(|(n, a)| (*n, clap.embed_audio(a).unwrap())).collect();
        assert!(vectors.iter().all(|(_, v)| v.len() == CLAP_EMBEDDING_DIM));
        let cos = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
        let mut table = std::collections::HashMap::new();
        for q in ["a deep punchy kick drum", "kick", "a bright hi-hat cymbal", "hi-hat", "warm analog pad", "pad"] {
            let t = clap.embed_text(q).unwrap();
            let row: Vec<(&str, f32)> = vectors.iter().map(|(n, v)| (*n, cos(&t, v))).collect();
            eprintln!("{q:28} {row:?}");
            table.insert(q, row);
        }
        let get = |q: &str, n: &str| table[q].iter().find(|(m, _)| *m == n).unwrap().1;
        for q in ["a deep punchy kick drum", "kick"] {
            assert!(get(q, "kick") > get(q, "hat"), "{q}: kick must rank above hat");
        }
        for q in ["a bright hi-hat cymbal", "hi-hat"] {
            assert!(get(q, "hat") > get(q, "kick"), "{q}: hat must rank above kick");
        }
        assert!(get("warm analog pad", "pad") > get("warm analog pad", "hat"));
    }

    /// Pins the tokenizer to the Hugging Face one: `Hello world` is `[0, 31414, 232, 2]`.
    #[test]
    #[ignore = "needs the CLAP model files (set AITRACK_CLAP_DIR to a folder with vocab.json and merges.txt)"]
    fn the_real_vocabulary_tokenizes_like_roberta() {
        let dir = std::path::PathBuf::from(std::env::var_os("AITRACK_CLAP_DIR").expect("AITRACK_CLAP_DIR"));
        let t = Tokenizer::from_files(&dir.join("vocab.json"), &dir.join("merges.txt")).unwrap();
        assert_eq!(t.encode("Hello world", 77), vec![0, 31414, 232, 2]);
        assert_eq!(t.encode("hello world", 77), vec![0, 42891, 232, 2]);
        let kick = t.encode("a dark punchy kick drum", 77);
        assert_eq!((kick[0], *kick.last().unwrap()), (0, 2));
        assert!(kick.len() >= 6 && !kick.contains(&3), "{kick:?}");
    }
}
