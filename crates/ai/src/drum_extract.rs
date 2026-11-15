//! Clean drum stem, one-shot, and loop extraction from mixed tracks (WAV/MP3/FLAC…).
//!
//! Combines multi-resolution Harmonic-Percussive Source Separation (HPSS with
//! Wiener soft masking, FitzGerald 2010 / Driedger et al. 2014), Mid/Side
//! de-bleed, spectral de-reverb & transient shaping (inspired by iZotope RX
//! Music Rebalance / De-bleed / Spectral Repair and Accusonus Regroover),
//! optional HT-Demucs neural stem pre-separation, and class-specific spectral
//! cleanup for each extracted one-shot (`Kick`, `Snare`, `Clap`, `ClosedHat`,
//! `OpenHat`, `Tom`, `Perc`, `Crash`) plus bar-aligned seamless drum loops.

use std::collections::BTreeMap;
use std::f32::consts::PI;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};
use rustfft::num_complex::Complex32;
use serde::{Deserialize, Serialize};

use crate::audio::{self, WavFormat, resample_sinc, write_wav};
use crate::dsp::{ANALYSIS_RATE, OnsetEnvelope, Spectrogram};
use crate::models::{ModelManager, ids};
use crate::progress::{CancelToken, Progress, Stage};
use crate::stems::{StemOptions, split_stems_with};
use crate::tempo;
use crate::{Error, Result};

const WORK_RATE: u32 = 44_100;
const N_FFT: usize = 2048;
const HOP: usize = 512;
const TIME_MEDIAN: usize = 17;
const FREQ_MEDIAN: usize = 17;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtractionEngine {
    /// Use HT-Demucs if installed & license accepted, then RX-style DSP cleanup;
    /// otherwise run the pure DSP HPSS + De-bleed pipeline without downloads.
    #[default]
    Auto,
    /// Zero-model pure DSP HPSS + Mid/Side De-bleed + Spectral De-reverb.
    PureDsp,
    /// Require HT-Demucs stem isolation followed by RX-style DSP cleanup.
    DemucsHybrid,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DrumClass {
    Kick,
    Snare,
    Clap,
    ClosedHat,
    OpenHat,
    Tom,
    Perc,
    Crash,
}

impl DrumClass {
    pub const ALL: [Self; 8] = [
        Self::Kick,
        Self::Snare,
        Self::Clap,
        Self::ClosedHat,
        Self::OpenHat,
        Self::Tom,
        Self::Perc,
        Self::Crash,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Kick => "Kick",
            Self::Snare => "Snare",
            Self::Clap => "Clap",
            Self::ClosedHat => "Closed Hat",
            Self::OpenHat => "Open Hat",
            Self::Tom => "Tom",
            Self::Perc => "Perc",
            Self::Crash => "Crash",
        }
    }

    pub fn folder_name(self) -> &'static str {
        match self {
            Self::Kick => "Kicks",
            Self::Snare => "Snares",
            Self::Clap => "Claps",
            Self::ClosedHat => "Closed Hats",
            Self::OpenHat => "Open Hats",
            Self::Tom => "Toms",
            Self::Perc => "Percs",
            Self::Crash => "Crashes",
        }
    }

    pub fn midi_note(self) -> u8 {
        match self {
            Self::Kick => 36,
            Self::Snare => 38,
            Self::Clap => 39,
            Self::ClosedHat => 42,
            Self::OpenHat => 46,
            Self::Tom => 45,
            Self::Perc => 56,
            Self::Crash => 49,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DrumExtractOptions {
    pub engine: ExtractionEngine,
    /// Strength of harmonic/side/neighbor de-bleed (0.0 ..= 1.0).
    pub de_bleed: f32,
    /// Strength of spectral de-reverb tail cleanup (0.0 ..= 1.0).
    pub de_reverb: f32,
    /// Transient attack punch enhancement (0.0 ..= 1.0).
    pub transient_focus: f32,
    /// Onset slice sensitivity (0.0 ..= 1.0).
    pub sensitivity: f32,
    /// Number of bars per extracted drum loop (1, 2, or 4).
    pub loop_bars: u8,
    /// Target peak normalization in dBFS (e.g. -1.0); `None` leaves raw gain.
    pub normalize_peak_db: Option<f32>,
    /// Output WAV bit depth.
    pub format: WavFormat,
}

impl Default for DrumExtractOptions {
    fn default() -> Self {
        Self {
            engine: ExtractionEngine::Auto,
            de_bleed: 0.75,
            de_reverb: 0.45,
            transient_focus: 0.35,
            sensitivity: 0.60,
            loop_bars: 2,
            normalize_peak_db: Some(-1.0),
            format: WavFormat::Pcm24,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExtractedOneShot {
    pub id: usize,
    pub name: String,
    pub class: DrumClass,
    pub time_sec: f64,
    pub duration_sec: f64,
    pub peak_db: f32,
    /// Estimated percussive isolation quality (0.0 ..= 1.0).
    pub isolation_score: f32,
    pub path: PathBuf,
    /// Compact 32-point waveform envelope (0.0 ..= 1.0) for UI previews.
    pub peaks: Vec<f32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExtractedLoop {
    pub name: String,
    pub bpm: f32,
    pub bars: u8,
    pub start_sec: f64,
    pub duration_sec: f64,
    pub isolation_score: f32,
    pub path: PathBuf,
    /// Compact 64-point waveform envelope (0.0 ..= 1.0) for UI previews.
    pub peaks: Vec<f32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DrumExtractionResult {
    pub source_path: PathBuf,
    pub source_name: String,
    pub engine_used: String,
    pub sample_rate: u32,
    pub duration_sec: f64,
    pub bpm: Option<f32>,
    pub clean_stem_path: PathBuf,
    pub oneshots: Vec<ExtractedOneShot>,
    pub loops: Vec<ExtractedLoop>,
    pub out_dir: PathBuf,
}

/// Extract clean isolated drum stem, individual one-shots (one by one), and
/// seamless bar-aligned loops from `path`, writing WAVs under `out_dir`.
pub fn extract_drums(
    path: impl AsRef<Path>,
    out_dir: impl AsRef<Path>,
    opts: &DrumExtractOptions,
    progress: &mut dyn FnMut(Progress),
    cancel: &CancelToken,
) -> Result<DrumExtractionResult> {
    let manager = ModelManager::new()?;
    extract_drums_with(&manager, path.as_ref(), out_dir.as_ref(), opts, progress, cancel)
}

pub fn extract_drums_with(
    manager: &ModelManager,
    path: &Path,
    out_dir: &Path,
    opts: &DrumExtractOptions,
    progress: &mut dyn FnMut(Progress),
    cancel: &CancelToken,
) -> Result<DrumExtractionResult> {
    progress(Progress::new(Stage::Decoding, 0.02, "Decoding source track"));
    let src = audio::load(path)?;
    if src.frames() == 0 {
        return Err(Error::InvalidInput("empty audio file".into()));
    }
    cancel.check()?;

    let source_name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Track".to_owned());

    std::fs::create_dir_all(out_dir).map_err(|e| Error::io(out_dir, e))?;

    let use_demucs = match opts.engine {
        ExtractionEngine::PureDsp => false,
        ExtractionEngine::DemucsHybrid => true,
        ExtractionEngine::Auto => demucs_ready(manager),
    };

    let (base_stereo, original_rate, engine_used) = if use_demucs {
        progress(Progress::new(Stage::Inference, 0.08, "Isolating drums with HT-Demucs"));
        let stem_dir = out_dir.join("_demucs");
        let stem_opts = StemOptions {
            model: ids::HTDEMUCS.into(),
            format: WavFormat::Float32,
            only: Some(vec!["drums".into()]),
            ..StemOptions::default()
        };
        let files = split_stems_with(
            manager,
            path,
            &stem_dir,
            &stem_opts,
            &mut |p| {
                progress(Progress {
                    fraction: 0.08 + p.fraction * 0.42,
                    ..p
                })
            },
            cancel,
        )?;
        let drum_wav = files
            .into_iter()
            .next()
            .ok_or_else(|| Error::ModelOutput("HT-Demucs did not produce a drums stem".into()))?;
        let drum_audio = audio::load(&drum_wav)?;
        let _ = std::fs::remove_dir_all(&stem_dir);
        (drum_audio.to_stereo(), drum_audio.sample_rate.max(WORK_RATE), "HT-Demucs + RX Cleanup".to_owned())
    } else {
        (src.to_stereo(), src.sample_rate, "RX HPSS + Spectral De-Bleed".to_owned())
    };
    cancel.check()?;

    let work_rate = WORK_RATE;
    let work_stereo = if original_rate != work_rate {
        progress(Progress::new(Stage::PostProcessing, 0.50, "Resampling for spectral analysis"));
        let mut r = resample_sinc(&base_stereo, original_rate, work_rate)?;
        let r_ch = r.pop().unwrap_or_default();
        let l_ch = r.pop().unwrap_or_default();
        [l_ch, r_ch]
    } else {
        base_stereo
    };
    cancel.check()?;

    progress(Progress::new(Stage::PostProcessing, 0.56, "Running HPSS, De-Bleed & De-Reverb"));
    let (clean_stereo, isolation_map) = clean_percussive_stereo(&work_stereo, work_rate, opts, cancel)?;
    cancel.check()?;

    let out_rate = src.sample_rate.max(WORK_RATE);
    let mut stem_norm = if out_rate != work_rate {
        let mut r = resample_sinc(&clean_stereo, work_rate, out_rate)?;
        let r_ch = r.pop().unwrap_or_default();
        let l_ch = r.pop().unwrap_or_default();
        [l_ch, r_ch]
    } else {
        clean_stereo.clone()
    };

    let stem_path = out_dir.join(format!("{source_name} - Clean Drums.wav"));
    if let Some(target_db) = opts.normalize_peak_db {
        normalize_stereo(&mut stem_norm, target_db);
    }
    write_wav(&stem_path, &stem_norm, out_rate, opts.format)?;

    progress(Progress::new(Stage::PostProcessing, 0.72, "Detecting drum hits & classifying"));
    let clean_mono = downmix_stereo(&clean_stereo);
    let bpm_est = detect_clean_bpm(&clean_mono, work_rate);
    let bpm = bpm_est.map(|b| b.bpm);

    let oneshots = slice_and_clean_oneshots(
        &clean_stereo,
        &clean_mono,
        &isolation_map,
        work_rate,
        out_dir,
        opts,
        cancel,
    )?;
    cancel.check()?;

    progress(Progress::new(Stage::PostProcessing, 0.88, "Extracting bar-aligned drum loops"));
    let loops = extract_clean_loops(
        &clean_stereo,
        &isolation_map,
        work_rate,
        bpm,
        &source_name,
        out_dir,
        opts,
        cancel,
    )?;

    progress(Progress::new(Stage::Done, 1.0, format!("Extracted {} one-shots and {} loops", oneshots.len(), loops.len())));

    Ok(DrumExtractionResult {
        source_path: path.to_path_buf(),
        source_name,
        engine_used,
        sample_rate: work_rate,
        duration_sec: clean_mono.len() as f64 / work_rate as f64,
        bpm,
        clean_stem_path: stem_path,
        oneshots,
        loops,
        out_dir: out_dir.to_path_buf(),
    })
}

fn demucs_ready(manager: &ModelManager) -> bool {
    let Ok(entry) = manager.entry(ids::HTDEMUCS) else {
        return false;
    };
    if entry.requires_acceptance && !manager.is_accepted(ids::HTDEMUCS) {
        return false;
    }
    manager.installed(ids::HTDEMUCS).is_some()
        && (manager.installed(ids::ONNXRUNTIME).is_some() || std::env::var_os("AITRACK_ORT_DYLIB").is_some())
}

fn downmix_stereo(stereo: &[Vec<f32>; 2]) -> Vec<f32> {
    let n = stereo[0].len().min(stereo[1].len());
    (0..n).map(|i| 0.5 * (stereo[0][i] + stereo[1][i])).collect()
}

/// Core RX-inspired DSP cleaning pass:
/// 1) STFT on L/R channels
/// 2) 2D median HPSS (time median = harmonic, freq median = percussive)
/// 3) Sub-bass harmonic guard (<140 Hz) to strip sustained basslines while keeping kicks
/// 4) Mid/Side de-bleed for wide melodic/vocal bleed
/// 5) Spectral de-reverb floor subtraction (smoothed across bins)
/// 6) Transient attack focus
fn clean_percussive_stereo(
    stereo: &[Vec<f32>; 2],
    sr: u32,
    opts: &DrumExtractOptions,
    cancel: &CancelToken,
) -> Result<([Vec<f32>; 2], Vec<f32>)> {
    let n_samples = stereo[0].len().min(stereo[1].len());
    if n_samples == 0 {
        return Ok(([Vec::new(), Vec::new()], Vec::new()));
    }

    let mut planner = RealFftPlanner::<f32>::new();
    let fft: Arc<dyn RealToComplex<f32>> = planner.plan_fft_forward(N_FFT);
    let ifft: Arc<dyn ComplexToReal<f32>> = planner.plan_fft_inverse(N_FFT);
    let win = hann_window(N_FFT);
    let n_bins = N_FFT / 2 + 1;

    let n_frames = if n_samples <= N_FFT {
        1
    } else {
        1 + (n_samples - N_FFT).div_ceil(HOP)
    };

    let mut spec_l = vec![vec![Complex32::new(0.0, 0.0); n_bins]; n_frames];
    let mut spec_r = vec![vec![Complex32::new(0.0, 0.0); n_bins]; n_frames];
    let mut power = vec![vec![0.0f32; n_bins]; n_frames];

    let mut in_buf = fft.make_input_vec();
    let mut out_buf = fft.make_output_vec();

    for t in 0..n_frames {
        if t % 64 == 0 {
            cancel.check()?;
        }
        let start = t * HOP;
        for (ch, dst) in [(&stereo[0], &mut spec_l[t]), (&stereo[1], &mut spec_r[t])] {
            for i in 0..N_FFT {
                in_buf[i] = ch.get(start + i).copied().unwrap_or(0.0) * win[i];
            }
            fft.process(&mut in_buf, &mut out_buf).expect("fft size");
            dst.copy_from_slice(&out_buf);
        }
        for k in 0..n_bins {
            let ml = spec_l[t][k].norm_sqr();
            let mr = spec_r[t][k].norm_sqr();
            power[t][k] = (0.5 * (ml + mr)).sqrt();
        }
    }

    // Horizontal median (along time -> harmonic) and frequency-adaptive vertical
    // median (along freq -> percussive). Kicks occupy only a few bins below
    // 280 Hz, so the vertical median uses a narrow 5-bin window in the low end
    // and the wider FREQ_MEDIAN above 280 Hz.
    let half_t = TIME_MEDIAN / 2;
    let low_adaptive_limit = ((280.0 * N_FFT as f32 / sr as f32).round() as usize).min(n_bins);
    let mut harm = vec![vec![0.0f32; n_bins]; n_frames];
    let mut perc = vec![vec![0.0f32; n_bins]; n_frames];
    let mut scratch = Vec::with_capacity(TIME_MEDIAN.max(FREQ_MEDIAN));

    for k in 0..n_bins {
        for (t, h_frame) in harm.iter_mut().enumerate() {
            scratch.clear();
            let a = t.saturating_sub(half_t);
            let b = (t + half_t + 1).min(n_frames);
            for p_frame in &power[a..b] {
                scratch.push(p_frame[k]);
            }
            h_frame[k] = median_of(&mut scratch);
        }
    }
    for (t, (p_frame, perc_frame)) in power.iter().zip(perc.iter_mut()).enumerate() {
        if t % 64 == 0 {
            cancel.check()?;
        }
        for (k, slot) in perc_frame.iter_mut().enumerate() {
            let half_f = if k < low_adaptive_limit { 2 } else { FREQ_MEDIAN / 2 };
            scratch.clear();
            let a = k.saturating_sub(half_f);
            let b = (k + half_f + 1).min(n_bins);
            scratch.extend_from_slice(&p_frame[a..b]);
            *slot = median_of(&mut scratch);
        }
    }

    // Broadband percussive flux per frame for transient detection & sub-bass kick guard.
    let mut frame_flux = vec![0.0f32; n_frames];
    for t in 1..n_frames {
        let s: f32 = perc[t]
            .iter()
            .zip(&perc[t - 1])
            .skip(1)
            .map(|(&cur, &prev)| ((1.0 + 40.0 * cur).ln() - (1.0 + 40.0 * prev).ln()).max(0.0))
            .sum();
        frame_flux[t] = s / n_bins as f32;
    }
    let mean_flux = frame_flux.iter().sum::<f32>() / n_frames.max(1) as f32;

    let de_bleed = opts.de_bleed.clamp(0.0, 1.0);
    let de_reverb = opts.de_reverb.clamp(0.0, 1.0);
    let transient = opts.transient_focus.clamp(0.0, 1.0);
    let wiener_p = 1.6 + 1.8 * de_bleed;
    let sub_bin_limit = ((140.0 * N_FFT as f32 / sr as f32).round() as usize).min(n_bins);

    // Running diffuse reverb spectrum estimate + smoothed spectral gains.
    let mut reverb_floor = vec![0.0f32; n_bins];
    let mut raw_gain = vec![1.0f32; n_bins];
    let mut isolation_per_frame = vec![0.0f32; n_frames];

    for t in 0..n_frames {
        if t % 64 == 0 {
            cancel.check()?;
        }
        let is_attack = frame_flux[t] > mean_flux * 1.45
            || (t > 0 && frame_flux[t - 1] > mean_flux * 1.45)
            || (t + 1 < n_frames && frame_flux[t + 1] > mean_flux * 1.45);
        let attack_boost = if is_attack { 1.0 + 0.55 * transient } else { 1.0 - 0.15 * transient };

        let mut perc_e = 0.0f32;
        let mut total_e = 1e-12f32;

        for k in 0..n_bins {
            let p_val = perc[t][k].powf(wiener_p);
            let h_val = harm[t][k].powf(wiener_p);
            let mut m_p = p_val / (p_val + h_val + 1e-12);

            // Guard low end (< 140 Hz) against sustained bass notes between kicks.
            if k > 0 && k < sub_bin_limit && de_bleed > 0.1 && !is_attack && harm[t][k] > perc[t][k] * 1.1 {
                m_p *= 1.0 - 0.72 * de_bleed;
            }

            let perc_mag = power[t][k] * m_p;
            perc_e += perc_mag * perc_mag;
            total_e += power[t][k] * power[t][k];

            // Spectral de-reverb: attenuate persistent diffuse tail relative to recent history.
            let rev_g = if de_reverb > 0.01 {
                let floor_sub = (perc_mag - 0.78 * de_reverb * reverb_floor[k]).max(0.0);
                let g = floor_sub / (perc_mag + 1e-9);
                g.max(1.0 - 0.85 * de_reverb)
            } else {
                1.0
            };
            reverb_floor[k] = (reverb_floor[k] * 0.86).max(perc_mag * 0.65);

            // Blend raw vs HPSS mask according to de_bleed so de_bleed=0 passes cleanly.
            let hpss_g = (1.0 - de_bleed) + de_bleed * m_p;
            raw_gain[k] = (hpss_g * rev_g * attack_boost).clamp(0.0, 1.8);
        }

        isolation_per_frame[t] = (perc_e / total_e).sqrt().clamp(0.0, 1.0);

        // 3-bin spectral smoothing prevents musical noise artifacts.
        let side_keep = 1.0 - 0.45 * de_bleed * (1.0 - isolation_per_frame[t]);
        for k in 0..n_bins {
            let g0 = raw_gain[k.saturating_sub(1)];
            let g1 = raw_gain[k];
            let g2 = raw_gain[(k + 1).min(n_bins - 1)];
            let g = 0.25 * g0 + 0.5 * g1 + 0.25 * g2;

            let l = spec_l[t][k];
            let r = spec_r[t][k];
            let mid = (l + r) * 0.5;
            let side = (l - r) * 0.5 * side_keep;
            spec_l[t][k] = (mid + side) * g;
            spec_r[t][k] = (mid - side) * g;
        }
        // DC and Nyquist bins stay purely real for ComplexToReal.
        spec_l[t][0].im = 0.0;
        spec_r[t][0].im = 0.0;
        spec_l[t][n_bins - 1].im = 0.0;
        spec_r[t][n_bins - 1].im = 0.0;
    }

    // Inverse STFT with exact COLA window-squared normalization.
    let mut out_l = vec![0.0f32; n_samples];
    let mut out_r = vec![0.0f32; n_samples];
    let mut norm = vec![0.0f32; n_samples];
    let mut spec_buf = ifft.make_input_vec();
    let mut time_buf = ifft.make_output_vec();
    let fft_scale = 1.0 / N_FFT as f32;

    for t in 0..n_frames {
        let start = t * HOP;
        for (src, dst) in [(&spec_l[t], &mut out_l), (&spec_r[t], &mut out_r)] {
            spec_buf.copy_from_slice(src);
            ifft.process(&mut spec_buf, &mut time_buf).expect("ifft size");
            for i in 0..N_FFT {
                if start + i < n_samples {
                    dst[start + i] += time_buf[i] * fft_scale * win[i];
                }
            }
        }
        for i in 0..N_FFT {
            if start + i < n_samples {
                norm[start + i] += win[i] * win[i];
            }
        }
    }
    for i in 0..n_samples {
        let inv = if norm[i] > 1e-6 { 1.0 / norm[i] } else { 1.0 };
        out_l[i] *= inv;
        out_r[i] *= inv;
    }

    Ok(([out_l, out_r], isolation_per_frame))
}

fn hann_window(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| 0.5 - 0.5 * (2.0 * PI * i as f32 / n as f32).cos())
        .collect()
}

fn median_of(v: &mut [f32]) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    let mid = v.len() / 2;
    let (_, m, _) = v.select_nth_unstable_by(mid, |a, b| a.total_cmp(b));
    *m
}

fn detect_clean_bpm(mono: &[f32], sr: u32) -> Option<tempo::BpmEstimate> {
    let down = audio::resample_channel(mono, sr, ANALYSIS_RATE).ok()?;
    tempo::detect_bpm(&down, ANALYSIS_RATE)
}

fn slice_and_clean_oneshots(
    stereo: &[Vec<f32>; 2],
    mono: &[f32],
    isolation_frames: &[f32],
    sr: u32,
    out_dir: &Path,
    opts: &DrumExtractOptions,
    cancel: &CancelToken,
) -> Result<Vec<ExtractedOneShot>> {
    let n_samples = mono.len();
    if n_samples < 256 {
        return Ok(Vec::new());
    }
    let down = audio::resample_channel(mono, sr, ANALYSIS_RATE)?;
    let hop_a = 128usize;
    let n_fft_a = 1024usize;
    let pad = 1024usize;
    let mut padded = vec![0.0f32; pad];
    padded.extend_from_slice(&down);

    let spec = Spectrogram::compute(&padded, ANALYSIS_RATE, n_fft_a, hop_a);
    let env_full = OnsetEnvelope::from_spectrogram(&spec, 30.0, ANALYSIS_RATE as f32 * 0.5);
    let env_high = OnsetEnvelope::from_spectrogram(&spec, 3500.0, ANALYSIS_RATE as f32 * 0.5);
    let sens = opts.sensitivity.clamp(0.0, 1.0);
    // Threshold scales from strict (0.14) to sensitive (0.035), min gap 45 ms.
    let thresh = 0.14 - 0.105 * sens;
    let mut peaks = env_full.peaks(thresh, 0.045);
    for hp in env_high.peaks((thresh * 1.15).clamp(0.03, 0.25), 0.045) {
        if peaks.iter().all(|&p| p.abs_diff(hp) > 4) {
            peaks.push(hp);
        }
    }
    peaks.sort_unstable();
    if peaks.is_empty() {
        return Ok(Vec::new());
    }

    let b_sub = spec.hz_bin(190.0);
    let b_mid_lo = spec.hz_bin(200.0);
    let b_mid_hi = spec.hz_bin(2500.0);
    let b_hi = spec.hz_bin(3500.0);
    let ratio_sr = sr as f64 / ANALYSIS_RATE as f64;

    // Collect onset sample positions in work-rate audio.
    let mut onsets: Vec<(usize, usize)> = Vec::with_capacity(peaks.len());
    for &p in &peaks {
        let a_sample = (p * hop_a + 768).saturating_sub(pad);
        let approx = ((a_sample as f64 * ratio_sr).round() as usize).min(n_samples.saturating_sub(1));
        let start = snap_to_transient_start(mono, approx, sr);
        if onsets.last().is_some_and(|&(prev, _)| start.saturating_sub(prev) < (0.04 * sr as f64) as usize) {
            continue;
        }
        onsets.push((start, p));
    }

    let oneshots_dir = out_dir.join("oneshots");
    std::fs::create_dir_all(&oneshots_dir).map_err(|e| Error::io(&oneshots_dir, e))?;

    let mut class_counts: BTreeMap<DrumClass, usize> = BTreeMap::new();
    let mut results = Vec::new();
    let n_att = ((0.035 * ANALYSIS_RATE as f64) as usize / hop_a).max(3);

    for (idx, &(start, p)) in onsets.iter().enumerate() {
        if idx % 16 == 0 {
            cancel.check()?;
        }
        let next_start = onsets.get(idx + 1).map_or(n_samples, |&(s, _)| s);
        let class = classify_hit(&spec, &padded, p, n_att, b_sub, b_mid_lo, b_mid_hi, b_hi, start, next_start, sr);
        let max_len = match class {
            DrumClass::Kick | DrumClass::Crash | DrumClass::OpenHat => (1.1 * sr as f64) as usize,
            DrumClass::Snare | DrumClass::Tom => (0.65 * sr as f64) as usize,
            DrumClass::Clap | DrumClass::ClosedHat | DrumClass::Perc => (0.35 * sr as f64) as usize,
        };
        let guard = (0.004 * sr as f64) as usize;
        let hard_end = next_start.saturating_sub(guard).min(start + max_len).min(n_samples);
        if hard_end <= start + (0.015 * sr as f64) as usize {
            continue;
        }

        let end = find_decay_end(mono, start, hard_end, sr);
        let len = end - start;
        if len < (0.018 * sr as f64) as usize {
            continue;
        }

        let mut slice = [stereo[0][start..end].to_vec(), stereo[1][start..end].to_vec()];
        clean_hit_by_class(&mut slice, sr, class, opts.de_bleed);
        remove_dc(&mut slice);
        apply_hit_fades(&mut slice, sr);

        let raw_peak = peak_linear(&slice);
        if raw_peak < 1e-4 {
            continue;
        }
        if let Some(target_db) = opts.normalize_peak_db {
            normalize_stereo(&mut slice, target_db);
        }
        let peak_db = 20.0 * peak_linear(&slice).max(1e-6).log10();

        let f_a = (start / HOP).min(isolation_frames.len().saturating_sub(1));
        let f_b = ((start + (len / 3)) / HOP).min(isolation_frames.len().saturating_sub(1)).max(f_a);
        let iso = if isolation_frames.is_empty() {
            0.75
        } else {
            let slice_iso = &isolation_frames[f_a..=f_b];
            slice_iso.iter().sum::<f32>() / slice_iso.len() as f32
        };

        let count = class_counts.entry(class).or_insert(0);
        *count += 1;
        let name = format!("{} {:02}", class.label(), *count);
        let file_name = format!("{}_{:02}.wav", class.label().replace(' ', "_"), *count);
        let class_dir = oneshots_dir.join(class.folder_name());
        std::fs::create_dir_all(&class_dir).map_err(|e| Error::io(&class_dir, e))?;
        let wav_path = class_dir.join(&file_name);

        write_wav(&wav_path, &slice, sr, opts.format)?;

        let peaks_env = waveform_envelope(&slice, 32);
        results.push(ExtractedOneShot {
            id: results.len() + 1,
            name,
            class,
            time_sec: start as f64 / sr as f64,
            duration_sec: len as f64 / sr as f64,
            peak_db,
            isolation_score: iso,
            path: wav_path,
            peaks: peaks_env,
        });
    }

    Ok(results)
}

fn snap_to_transient_start(mono: &[f32], approx: usize, sr: u32) -> usize {
    let back = (0.008 * sr as f64) as usize;
    let fwd = (0.006 * sr as f64) as usize;
    let lo = approx.saturating_sub(back);
    let hi = (approx + fwd).min(mono.len());
    if hi <= lo {
        return approx.min(mono.len().saturating_sub(1));
    }
    let peak = mono[lo..hi].iter().fold(0.0f32, |m, &x| m.max(x.abs()));
    if peak < 1e-6 {
        return lo;
    }
    let thresh = peak * 0.08;
    let onset = mono[lo..hi]
        .iter()
        .position(|&x| x.abs() >= thresh)
        .map_or(lo, |off| lo + off);
    // Walk back slightly to nearest zero crossing (~1.5 ms pre-roll max).
    let zc_back = (0.0015 * sr as f64) as usize;
    let z_lo = onset.saturating_sub(zc_back);
    for i in (z_lo + 1..=onset).rev() {
        if (mono[i - 1] <= 0.0 && mono[i] >= 0.0) || (mono[i - 1] >= 0.0 && mono[i] <= 0.0) {
            return i;
        }
    }
    z_lo
}

fn find_decay_end(mono: &[f32], start: usize, hard_end: usize, sr: u32) -> usize {
    let win = (0.015 * sr as f64) as usize;
    let peak = mono[start..(start + win * 3).min(hard_end)]
        .iter()
        .fold(0.0f32, |m, &x| m.max(x.abs()));
    let silence_thresh = (peak * 0.005).max(1e-4);
    let min_hold = (0.025 * sr as f64) as usize;
    let mut quiet = 0usize;
    for (offset, &s) in mono[(start + win)..hard_end].iter().enumerate() {
        if s.abs() < silence_thresh {
            quiet += 1;
            if quiet >= min_hold {
                return start + win + offset;
            }
        } else {
            quiet = 0;
        }
    }
    hard_end
}

#[allow(clippy::too_many_arguments)]
fn classify_hit(
    spec: &Spectrogram,
    padded: &[f32],
    p: usize,
    n_att: usize,
    b_sub: usize,
    b_mid_lo: usize,
    b_mid_hi: usize,
    b_hi: usize,
    start: usize,
    next_start: usize,
    sr: u32,
) -> DrumClass {
    let before = &spec.frames[p.saturating_sub(3)];
    let after_start = (p + 1).min(spec.frames.len().saturating_sub(1));
    let after: Vec<&Vec<f32>> = spec.frames.iter().skip(after_start).take(n_att).collect();
    let (mut sub_e, mut mid_e, mut hi_e) = (0.0f64, 0.0f64, 0.0f64);
    let mut centroid_num = 0.0f64;
    let mut centroid_den = 1e-20f64;
    let mut log_sum = 0.0f64;
    let mut arith_sum = 0.0f64;
    let mut flat_n = 0usize;

    for b in 1..before.len() {
        let mean = after.iter().map(|f| (f[b] as f64).powi(2)).sum::<f64>() / after.len().max(1) as f64;
        let e = (mean - (before[b] as f64).powi(2)).max(0.0);
        let hz = spec.bin_hz(b) as f64;
        centroid_num += hz * e;
        centroid_den += e;
        if b >= b_mid_lo && b < b_hi {
            log_sum += (e + 1e-12).ln();
            arith_sum += e + 1e-12;
            flat_n += 1;
        }
        if b < b_sub {
            sub_e += e;
        } else if b < b_mid_hi {
            mid_e += e;
        } else {
            hi_e += e;
        }
    }

    let tot = (sub_e + mid_e + hi_e).max(1e-20);
    let (sub_r, mid_r, hi_r) = (sub_e / tot, mid_e / tot, hi_e / tot);
    let centroid = centroid_num / centroid_den;
    let flatness = if flat_n > 0 {
        (log_sum / flat_n as f64).exp() / (arith_sum / flat_n as f64)
    } else {
        0.0
    };

    // Check sustain after 150 ms in the analysis audio for open hat / crash detection.
    let onset_a = p * spec.hop + 768;
    let ring = sustain_ratio(padded, onset_a, (0.15 * ANALYSIS_RATE as f64) as usize);
    let gap_sec = next_start.saturating_sub(start) as f64 / sr as f64;

    if sub_r > 0.36 && sub_r > hi_r * 1.3 && centroid < 1200.0 {
        DrumClass::Kick
    } else if hi_r > 0.45 && sub_r < 0.20 {
        if ring > 0.12 && gap_sec > 0.25 {
            if centroid < 5200.0 && ring > 0.22 {
                DrumClass::Crash
            } else {
                DrumClass::OpenHat
            }
        } else {
            DrumClass::ClosedHat
        }
    } else if sub_r > 0.22 && mid_r > 0.35 && centroid < 1200.0 && flatness < 0.28 {
        DrumClass::Tom
    } else if mid_r + hi_r > 0.55 {
        if flatness > 0.36 && sub_r < 0.18 && centroid > 1800.0 {
            DrumClass::Clap
        } else if mid_r > 0.28 {
            DrumClass::Snare
        } else {
            DrumClass::Perc
        }
    } else if sub_r > 0.30 {
        DrumClass::Kick
    } else {
        DrumClass::Perc
    }
}

fn sustain_ratio(x: &[f32], at: usize, delay: usize) -> f32 {
    if at >= x.len() {
        return 0.0;
    }
    let peak = x[at..(at + 1800).min(x.len())].iter().fold(0.0f32, |m, &s| m.max(s.abs()));
    if peak < 1e-6 {
        return 0.0;
    }
    let t = at + delay;
    if t + 256 > x.len() {
        return 0.0;
    }
    let late = x[t..t + 256].iter().fold(0.0f32, |m, &s| m.max(s.abs()));
    late / peak
}

/// Apply class-specific de-bleed filtering so each extracted hit is crisp and free
/// of neighboring instrument rumble or hiss.
fn clean_hit_by_class(slice: &mut [Vec<f32>; 2], sr: u32, class: DrumClass, de_bleed: f32) {
    let amt = de_bleed.clamp(0.0, 1.0);
    if amt < 0.05 {
        return;
    }
    match class {
        DrumClass::Kick => {
            // Keep the first ~18 ms beater click intact, then roll off high-hat bleed above ~4.2 kHz.
            let click_end = ((0.018 * sr as f64) as usize).min(slice[0].len());
            let cutoff = 12_000.0 - 7_800.0 * amt;
            let a = (-2.0 * PI * cutoff / sr as f32).exp();
            for ch in slice.iter_mut() {
                let mut lp = ch[0];
                for (i, s) in ch.iter_mut().enumerate() {
                    lp = (1.0 - a) * *s + a * lp;
                    if i >= click_end {
                        let blend = ((i - click_end) as f32 / (0.01 * sr as f32)).min(1.0) * amt;
                        *s = *s * (1.0 - blend) + lp * blend;
                    }
                }
            }
        }
        DrumClass::Snare | DrumClass::Clap => {
            // High-pass at ~95 Hz to strip kick sub-bleed.
            let fc = 50.0 + 55.0 * amt;
            one_pole_hp_stereo(slice, sr, fc, amt);
        }
        DrumClass::ClosedHat | DrumClass::OpenHat => {
            // High-pass at ~2.2 kHz to isolate crisp cymbal air.
            let fc = 600.0 + 1800.0 * amt;
            one_pole_hp_stereo(slice, sr, fc, amt);
        }
        DrumClass::Crash => {
            let fc = 450.0 + 1000.0 * amt;
            one_pole_hp_stereo(slice, sr, fc, amt);
        }
        DrumClass::Tom => {
            let fc = 45.0 + 25.0 * amt;
            one_pole_hp_stereo(slice, sr, fc, amt);
        }
        DrumClass::Perc => {
            let fc = 80.0 + 120.0 * amt;
            one_pole_hp_stereo(slice, sr, fc, amt);
        }
    }
}

fn one_pole_hp_stereo(slice: &mut [Vec<f32>; 2], sr: u32, fc: f32, blend: f32) {
    let a = (-2.0 * PI * fc / sr as f32).exp();
    for ch in slice.iter_mut() {
        let mut lp = 0.0f32;
        for s in ch.iter_mut() {
            lp = (1.0 - a) * *s + a * lp;
            let hp = *s - lp;
            *s = *s * (1.0 - blend) + hp * blend;
        }
    }
}

fn remove_dc(slice: &mut [Vec<f32>; 2]) {
    for ch in slice.iter_mut() {
        if ch.is_empty() {
            continue;
        }
        let mean = ch.iter().sum::<f32>() / ch.len() as f32;
        for s in ch.iter_mut() {
            *s -= mean;
        }
    }
}

fn apply_hit_fades(slice: &mut [Vec<f32>; 2], sr: u32) {
    let len = slice[0].len();
    let fade_in = ((0.0006 * sr as f64) as usize).min(len / 8);
    let fade_out = ((0.024 * sr as f64) as usize).min(len / 3);
    for ch in slice.iter_mut() {
        for (i, s) in ch.iter_mut().take(fade_in).enumerate() {
            *s *= i as f32 / fade_in.max(1) as f32;
        }
        for i in 0..fade_out {
            let idx = len - 1 - i;
            let t = i as f32 / fade_out.max(1) as f32;
            let g = 0.5 - 0.5 * (PI * t).cos();
            ch[idx] *= g;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn extract_clean_loops(
    stereo: &[Vec<f32>; 2],
    isolation_frames: &[f32],
    sr: u32,
    bpm: Option<f32>,
    source_name: &str,
    out_dir: &Path,
    opts: &DrumExtractOptions,
    cancel: &CancelToken,
) -> Result<Vec<ExtractedLoop>> {
    let bpm = bpm.unwrap_or(120.0).clamp(50.0, 200.0);
    let bars = opts.loop_bars.clamp(1, 8);
    let bar_samples = ((60.0 / bpm as f64) * 4.0 * sr as f64).round() as usize;
    let loop_samples = bar_samples * bars as usize;
    let n_samples = stereo[0].len().min(stereo[1].len());
    if n_samples < bar_samples || loop_samples < 1024 {
        return Ok(Vec::new());
    }

    // Actual length clamped if track is shorter than requested `bars`.
    let actual_bars = if n_samples >= loop_samples {
        bars
    } else {
        (n_samples / bar_samples).max(1) as u8
    };
    let win_len = bar_samples * actual_bars as usize;
    let step = (bar_samples / 4).max(512);
    let mono = downmix_stereo(stereo);

    // Score candidate loop windows by percussive RMS * isolation score * downbeat punch.
    let mut candidates: Vec<(usize, f32, f32)> = Vec::new();
    let mut start = 0usize;
    while start + win_len <= n_samples {
        cancel.check()?;
        let anchored = snap_to_transient_start(&mono, start, sr);
        let end = (anchored + win_len).min(n_samples);
        if end - anchored < win_len - 256 {
            break;
        }
        let rms = rms_of(&mono[anchored..end]);
        let f_a = (anchored / HOP).min(isolation_frames.len().saturating_sub(1));
        let f_b = (end / HOP).min(isolation_frames.len().saturating_sub(1)).max(f_a);
        let iso = if isolation_frames.is_empty() {
            0.75
        } else {
            let s = &isolation_frames[f_a..=f_b];
            s.iter().sum::<f32>() / s.len() as f32
        };
        let punch = peak_of(&mono[anchored..(anchored + (0.05 * sr as f64) as usize).min(end)]);
        let score = rms * (0.5 + iso) * (0.6 + punch);
        candidates.push((anchored, score, iso));
        start += step;
    }

    candidates.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut chosen: Vec<(usize, f32, f32)> = Vec::new();
    for c in candidates {
        if chosen.iter().all(|&(s, _, _)| c.0.abs_diff(s) >= win_len / 2) {
            chosen.push(c);
            if chosen.len() >= 2 {
                break;
            }
        }
    }

    let loops_dir = out_dir.join("loops");
    std::fs::create_dir_all(&loops_dir).map_err(|e| Error::io(&loops_dir, e))?;

    let mut out = Vec::new();
    for (idx, (s, _, iso)) in chosen.into_iter().enumerate() {
        let end = (s + win_len).min(n_samples);
        let mut loop_audio = [stereo[0][s..end].to_vec(), stereo[1][s..end].to_vec()];
        // Seamless loop seam crossfade (6 ms) using tail just before `s` when available.
        let seam = ((0.006 * sr as f64) as usize).min(win_len / 16);
        if s >= seam && seam > 0 {
            for ch in 0..2 {
                let len = loop_audio[ch].len();
                for i in 0..seam {
                    let t = i as f32 / seam as f32;
                    let g_end = (1.0 - t).sqrt();
                    let g_pre = t.sqrt();
                    let tail_sample = stereo[ch][s - seam + i];
                    loop_audio[ch][len - seam + i] = loop_audio[ch][len - seam + i] * g_end + tail_sample * g_pre;
                }
            }
        }
        remove_dc(&mut loop_audio);
        if let Some(target_db) = opts.normalize_peak_db {
            normalize_stereo(&mut loop_audio, target_db);
        }

        let tag = if idx == 0 { "Main".to_owned() } else { format!("Var {}", idx + 1) };
        let name = format!("{source_name} Drums {actual_bars}B {bpm:.0}BPM ({tag})");
        let file_name = format!("{source_name}_Drums_{actual_bars}Bar_{:.0}bpm_{}.wav", bpm, idx + 1);
        let wav_path = loops_dir.join(&file_name);
        write_wav(&wav_path, &loop_audio, sr, opts.format)?;
        out.push(ExtractedLoop {
            name,
            bpm,
            bars: actual_bars,
            start_sec: s as f64 / sr as f64,
            duration_sec: win_len as f64 / sr as f64,
            isolation_score: iso,
            path: wav_path,
            peaks: waveform_envelope(&loop_audio, 64),
        });
    }

    Ok(out)
}

fn peak_linear(stereo: &[Vec<f32>; 2]) -> f32 {
    stereo[0]
        .iter()
        .chain(&stereo[1])
        .fold(0.0f32, |m, &s| m.max(s.abs()))
}

fn peak_of(x: &[f32]) -> f32 {
    x.iter().fold(0.0f32, |m, &s| m.max(s.abs()))
}

fn rms_of(x: &[f32]) -> f32 {
    if x.is_empty() {
        return 0.0;
    }
    let sum = x.iter().map(|&s| s * s).sum::<f32>();
    (sum / x.len() as f32).sqrt()
}

fn normalize_stereo(stereo: &mut [Vec<f32>; 2], target_db: f32) {
    let peak = peak_linear(stereo);
    if peak < 1e-6 {
        return;
    }
    let target = 10f32.powf(target_db / 20.0);
    let gain = target / peak;
    for ch in stereo.iter_mut() {
        for s in ch.iter_mut() {
            *s *= gain;
        }
    }
}

fn waveform_envelope(stereo: &[Vec<f32>; 2], bars: usize) -> Vec<f32> {
    let n = stereo[0].len().min(stereo[1].len());
    if n == 0 || bars == 0 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(bars);
    for b in 0..bars {
        let a = b * n / bars;
        let z = ((b + 1) * n / bars).max(a + 1).min(n);
        let p = stereo[0][a..z]
            .iter()
            .zip(&stereo[1][a..z])
            .fold(0.0f32, |m, (&l, &r)| m.max(l.abs()).max(r.abs()));
        out.push(p.clamp(0.0, 1.0));
    }
    let max_p = out.iter().copied().fold(0.0f32, f32::max);
    if max_p > 1e-6 {
        for v in &mut out {
            *v /= max_p;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::synth;

    #[test]
    fn extracts_clean_oneshots_and_loop_from_mixed_track() {
        let sr = 44_100;
        let bpm = 120.0;
        let beat = 60.0 / bpm;
        let bars = 2usize;
        let total = (beat * 4.0 * bars as f64 * sr as f64) as usize;
        let mut mix_l = vec![0.0f32; total];
        let mut mix_r = vec![0.0f32; total];

        // Add sustained stereo harmonic synth + bass pad as "bleed" to strip.
        for i in 0..total {
            let t = i as f64 / sr as f64;
            let bass = (2.0 * std::f64::consts::PI * 55.0 * t).sin() as f32 * 0.22;
            let pad_l = (2.0 * std::f64::consts::PI * 330.0 * t).sin() as f32 * 0.14;
            let pad_r = (2.0 * std::f64::consts::PI * 440.0 * t + 0.5).sin() as f32 * 0.14;
            mix_l[i] += bass + pad_l;
            mix_r[i] += bass + pad_r;
        }

        // Layer kick, snare, and hi-hats across 2 bars.
        for b in 0..(4 * bars) {
            let at = (b as f64 * beat * sr as f64) as usize;
            if b % 2 == 0 {
                let k = synth::kick(sr, 0.30);
                synth::mix_at(&mut mix_l, &k, at);
                synth::mix_at(&mut mix_r, &k, at);
            } else {
                let s = synth::snare(sr, 0.22, b as u32);
                synth::mix_at(&mut mix_l, &s, at);
                synth::mix_at(&mut mix_r, &s, at);
            }
            let off = at + (beat * 0.5 * sr as f64) as usize;
            let h = synth::hat(sr, 0.05, 50 + b as u32);
            synth::mix_at(&mut mix_l, &h, off);
            synth::mix_at(&mut mix_r, &h, off);
        }

        let tmp = std::env::temp_dir().join(format!("aitrack-drum-extract-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let src_wav = tmp.join("mixed_track.wav");
        write_wav(&src_wav, &[mix_l, mix_r], sr, WavFormat::Float32).unwrap();

        let out_dir = tmp.join("extracted");
        let opts = DrumExtractOptions {
            engine: ExtractionEngine::PureDsp,
            loop_bars: 2,
            ..Default::default()
        };
        let res = extract_drums(&src_wav, &out_dir, &opts, &mut |_| {}, &CancelToken::new()).unwrap();

        assert!(res.clean_stem_path.exists());
        assert!(!res.oneshots.is_empty(), "expected extracted one-shots");
        assert!(res.oneshots.iter().any(|o| o.class == DrumClass::Kick), "expected at least one Kick");
        assert!(res.oneshots.iter().any(|o| matches!(o.class, DrumClass::Snare | DrumClass::Clap | DrumClass::Perc)));
        assert!(res.oneshots.iter().any(|o| matches!(o.class, DrumClass::ClosedHat | DrumClass::OpenHat)));
        assert!(res.oneshots.iter().all(|o| o.path.exists()));
        assert!(!res.loops.is_empty(), "expected at least one clean drum loop");
        assert!(res.loops.iter().all(|l| l.path.exists()));

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
