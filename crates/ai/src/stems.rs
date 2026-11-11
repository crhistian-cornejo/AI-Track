//! Stem separation with HT-Demucs exported to ONNX.
//!
//! The exported graph contains Demucs' own STFT/iSTFT (as convolutions), so
//! the host feeds raw stereo audio and reads raw stereo stems:
//!
//! * input  `mix`   `[1, 2, T]` float32, 44.1 kHz, T = 343 980 (7.8 s)
//! * output `stems` `[1, S, 2, T]`, S = 4 (drums, bass, other, vocals) or 6 (+ guitar, piano)
//!
//! Pipeline: decode → stereo → sinc-resample to the model rate → fixed-size
//! segments with overlap and linear cross-fades (optionally averaged over
//! time shifts, Demucs' `--shifts`) → overlap-add normalised by the window
//! sum → sinc-resample back to the source rate → trim/pad to exactly the input
//! length → WAV (24-bit by default, 32-bit float optional) named
//! `<song> - Drums.wav`, `<song> - Bass.wav`...
//!
//! Segment length and stem count come from the model's declared shapes when
//! static, falling back to the manifest.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::audio::{self, WavFormat, resample_sinc};
use crate::models::{ModelManager, ids};
use crate::progress::{CancelToken, Progress, Stage};
use crate::runtime::{self, Accel};
use crate::{Error, Result};

/// Default HT-Demucs segment: 7.8 s at 44.1 kHz.
pub const DEMUCS_SEGMENT: usize = 343_980;
pub const DEMUCS_RATE: u32 = 44_100;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StemOptions {
    /// Manifest id: `htdemucs` (4 stems) or `htdemucs-6s` (6 stems).
    pub model: String,
    /// WAV sample format. Default 24-bit integer.
    pub format: WavFormat,
    /// Overlap between segments, fraction of a segment (Demucs default 0.25).
    pub overlap: f32,
    /// Number of time-shifted passes to average (0/1 = single pass). Each pass costs a full run.
    pub shifts: u32,
    /// Write only these stems (by name, e.g. "vocals"); `None` = all.
    pub only: Option<Vec<String>>,
    pub accel: Accel,
}

impl Default for StemOptions {
    fn default() -> Self {
        Self {
            model: ids::HTDEMUCS.into(),
            format: WavFormat::Pcm24,
            overlap: 0.25,
            shifts: 1,
            only: None,
            accel: Accel::Auto,
        }
    }
}

/// Separate `path` into stems written to `out_dir`. The model must be
/// installed or its license notice accepted (see [`ModelManager::accept_license`]).
pub fn split_stems(
    path: impl AsRef<Path>,
    out_dir: impl AsRef<Path>,
    opts: &StemOptions,
    progress: &mut dyn FnMut(Progress),
    cancel: &CancelToken,
) -> Result<Vec<PathBuf>> {
    let manager = ModelManager::new()?;
    split_stems_with(
        &manager,
        path.as_ref(),
        out_dir.as_ref(),
        opts,
        progress,
        cancel,
    )
}

pub fn split_stems_with(
    manager: &ModelManager,
    path: &Path,
    out_dir: &Path,
    opts: &StemOptions,
    progress: &mut dyn FnMut(Progress),
    cancel: &CancelToken,
) -> Result<Vec<PathBuf>> {
    let entry = manager.entry(&opts.model)?.clone();
    progress(Progress::new(Stage::Decoding, 0.0, "Decoding audio"));
    let src = audio::load(path)?;
    let frames = src.frames();
    if frames == 0 {
        return Err(Error::InvalidInput("empty audio".into()));
    }
    cancel.check()?;
    let model = manager.ensure_cancellable(
        &opts.model,
        &mut |p| {
            progress(Progress {
                fraction: p.fraction * 0.1,
                ..p
            })
        },
        cancel,
    )?;
    progress(Progress::new(
        Stage::LoadingModel,
        0.1,
        format!("Loading {}", entry.name),
    ));
    let mut session = runtime::session(manager, &model.paths[0], opts.accel, &mut |p| {
        progress(Progress { fraction: 0.1, ..p })
    })?;
    let rate = if entry.sample_rate > 0 {
        entry.sample_rate
    } else {
        DEMUCS_RATE
    };
    let segment = runtime::input_shape(&session)
        .and_then(|s| s.last().copied())
        .filter(|d| *d > 0)
        .map_or(DEMUCS_SEGMENT, |d| d as usize);
    let n_src = runtime::output_shape(&session)
        .and_then(|s| s.get(1).copied())
        .filter(|d| *d > 0)
        .map_or(entry.stems.len(), |d| d as usize);
    let names: Vec<String> = (0..n_src)
        .map(|i| {
            entry
                .stems
                .get(i)
                .cloned()
                .unwrap_or_else(|| format!("stem{}", i + 1))
        })
        .collect();
    let input_name = session
        .inputs()
        .first()
        .map(|i| i.name().to_string())
        .ok_or_else(|| Error::ModelOutput("no inputs".into()))?;
    let output_name = session
        .outputs()
        .first()
        .map(|o| o.name().to_string())
        .ok_or_else(|| Error::ModelOutput("no outputs".into()))?;

    let stereo = src.to_stereo();
    let mix = resample_sinc(&stereo, src.sample_rate, rate)?;
    let mix = [mix[0].clone(), mix[1].clone()];
    cancel.check()?;

    let mut run = |chunk: &[f32]| -> Result<Vec<f32>> {
        let t = ort::value::Tensor::from_array(([1usize, 2, segment], chunk.to_vec()))?;
        let out = session.run(ort::inputs![input_name.as_str() => t])?;
        let v = out
            .get(output_name.as_str())
            .ok_or_else(|| Error::ModelOutput(format!("missing output {output_name}")))?;
        let (_, data) = v.try_extract_tensor::<f32>()?;
        Ok(data.to_vec())
    };
    let mut sep_progress =
        |f: f32, msg: String| progress(Progress::new(Stage::Inference, 0.12 + 0.75 * f, msg));
    let separated = separate(
        &mix,
        segment,
        n_src,
        opts.overlap,
        opts.shifts,
        &mut run,
        &mut sep_progress,
        cancel,
    )?;

    progress(Progress::new(Stage::Writing, 0.88, "Writing stems"));
    std::fs::create_dir_all(out_dir).map_err(|e| Error::io(out_dir, e))?;
    let song = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Song")
        .to_string();
    let mut written = Vec::new();
    for (i, (name, stem)) in names.iter().zip(separated).enumerate() {
        if opts
            .only
            .as_ref()
            .is_some_and(|o| !o.iter().any(|w| w.eq_ignore_ascii_case(name)))
        {
            continue;
        }
        cancel.check()?;
        let back = resample_sinc(&stem, rate, src.sample_rate)?;
        let channels = match_layout(back, src.channel_count(), frames);
        let file = out_dir.join(stem_file_name(&song, name));
        audio::write_wav(&file, &channels, src.sample_rate, opts.format)?;
        progress(Progress::new(
            Stage::Writing,
            0.88 + 0.12 * (i + 1) as f32 / n_src as f32,
            format!("Wrote {}", file.display()),
        ));
        written.push(file);
    }
    progress(Progress::new(
        Stage::Done,
        1.0,
        format!("{} stems", written.len()),
    ));
    Ok(written)
}

/// `<song> - Drums.wav`.
pub fn stem_file_name(song: &str, stem: &str) -> String {
    let mut c = stem.chars();
    let title: String = c
        .next()
        .map(|f| f.to_uppercase().chain(c).collect())
        .unwrap_or_default();
    format!("{song} - {title}.wav")
}

/// Exactly `frames` samples per channel, in the source's channel layout
/// (mono sources get the stereo stem averaged).
fn match_layout(stereo: Vec<Vec<f32>>, channels: usize, frames: usize) -> Vec<Vec<f32>> {
    let mut v: Vec<Vec<f32>> = if channels == 1 {
        vec![audio::downmix(&stereo)]
    } else {
        stereo
    };
    for ch in &mut v {
        ch.resize(frames, 0.0);
    }
    v
}

/// Chunked separation with overlap-add. `run` maps a planar `[2 × segment]`
/// chunk to a planar `[n_src × 2 × segment]` output. Returns `n_src` stereo
/// stems, each exactly as long as `mix`.
#[allow(clippy::too_many_arguments)]
pub fn separate(
    mix: &[Vec<f32>; 2],
    segment: usize,
    n_src: usize,
    overlap: f32,
    shifts: u32,
    run: &mut dyn FnMut(&[f32]) -> Result<Vec<f32>>,
    progress: &mut dyn FnMut(f32, String),
    cancel: &CancelToken,
) -> Result<Vec<Vec<Vec<f32>>>> {
    let total = mix[0].len().min(mix[1].len());
    let passes = shifts.max(1) as usize;
    // Demucs shifts the input by up to half a second between passes.
    let max_shift = (DEMUCS_RATE / 2) as usize;
    let mut acc: Vec<Vec<Vec<f32>>> = vec![vec![vec![0.0; total]; 2]; n_src];
    for pass in 0..passes {
        let offset = if passes > 1 {
            pass * max_shift / passes
        } else {
            0
        };
        let shifted: [Vec<f32>; 2] = std::array::from_fn(|c| {
            let mut v = vec![0.0f32; offset];
            v.extend_from_slice(&mix[c][..total]);
            v
        });
        let out = separate_once(
            &shifted,
            segment,
            n_src,
            overlap,
            run,
            &mut |f, m| progress((pass as f32 + f) / passes as f32, m),
            cancel,
        )?;
        for (a, o) in acc.iter_mut().zip(out) {
            for c in 0..2 {
                for (d, s) in a[c].iter_mut().zip(&o[c][offset..]) {
                    *d += s / passes as f32;
                }
            }
        }
    }
    Ok(acc)
}

fn separate_once(
    mix: &[Vec<f32>; 2],
    segment: usize,
    n_src: usize,
    overlap: f32,
    run: &mut dyn FnMut(&[f32]) -> Result<Vec<f32>>,
    progress: &mut dyn FnMut(f32, String),
    cancel: &CancelToken,
) -> Result<Vec<Vec<Vec<f32>>>> {
    let total = mix[0].len();
    let olap = ((segment as f32 * overlap.clamp(0.0, 0.5)) as usize).min(segment / 2);
    let stride = (segment - olap).max(1);
    let n_chunks = if total <= segment {
        1
    } else {
        1 + (total - segment).div_ceil(stride)
    };
    let mut out: Vec<Vec<Vec<f32>>> = vec![vec![vec![0.0; total]; 2]; n_src];
    let mut weight = vec![0.0f32; total];
    let mut input = vec![0.0f32; 2 * segment];
    for k in 0..n_chunks {
        cancel.check()?;
        progress(
            k as f32 / n_chunks as f32,
            format!("Separating segment {}/{n_chunks}", k + 1),
        );
        let start = k * stride;
        let end = (start + segment).min(total);
        let clen = end - start;
        for c in 0..2 {
            let dst = &mut input[c * segment..(c + 1) * segment];
            dst[..clen].copy_from_slice(&mix[c][start..end]);
            dst[clen..].fill(0.0);
        }
        let y = run(&input)?;
        let seg_out = y.len() / (n_src * 2).max(1);
        if y.len() != n_src * 2 * seg_out || seg_out < clen {
            return Err(Error::ModelOutput(format!(
                "expected [1, {n_src}, 2, {segment}], got {} values",
                y.len()
            )));
        }
        // Linear cross-fade on interior edges only, never reaching zero.
        let fade_in = start > 0;
        let fade_out = k + 1 < n_chunks;
        for i in 0..clen {
            let mut w = 1.0f32;
            if fade_in && i < olap {
                w = w.min((i + 1) as f32 / (olap + 1) as f32);
            }
            if fade_out && i >= segment - olap {
                w = w.min((segment - i) as f32 / (olap + 1) as f32);
            }
            weight[start + i] += w;
            for (s, src) in out.iter_mut().enumerate() {
                for (c, ch) in src.iter_mut().enumerate() {
                    ch[start + i] += y[(s * 2 + c) * seg_out + i] * w;
                }
            }
        }
    }
    for src in &mut out {
        for ch in src.iter_mut() {
            for (v, w) in ch.iter_mut().zip(&weight) {
                *v /= w.max(1e-8);
            }
        }
    }
    progress(1.0, "Separated".into());
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::synth;

    /// A fake "model": source s = mix * (s + 1) / 10, so overlap-add must reproduce that exactly.
    fn fake(n_src: usize, segment: usize) -> impl FnMut(&[f32]) -> Result<Vec<f32>> {
        move |x: &[f32]| {
            let mut y = vec![0.0f32; n_src * 2 * segment];
            for s in 0..n_src {
                for c in 0..2 {
                    for i in 0..segment {
                        y[(s * 2 + c) * segment + i] = x[c * segment + i] * (s + 1) as f32 / 10.0;
                    }
                }
            }
            Ok(y)
        }
    }

    #[test]
    fn overlap_add_reconstructs_and_keeps_length() {
        let seg = 1000;
        let n = 4321;
        let mix = [synth::noise(n, 1), synth::noise(n, 2)];
        for shifts in [1, 3] {
            let out = separate(
                &mix,
                seg,
                4,
                0.25,
                shifts,
                &mut fake(4, seg),
                &mut |_, _| {},
                &CancelToken::new(),
            )
            .unwrap();
            assert_eq!(out.len(), 4);
            for (s, stem) in out.iter().enumerate() {
                for c in 0..2 {
                    assert_eq!(stem[c].len(), n);
                    let k = (s + 1) as f32 / 10.0;
                    let err = stem[c]
                        .iter()
                        .zip(&mix[c])
                        .map(|(a, b)| (a - b * k).abs())
                        .fold(0.0f32, f32::max);
                    assert!(err < 1e-5, "stem {s} ch {c} shifts {shifts} err {err}");
                }
            }
        }
    }

    #[test]
    fn short_input_single_chunk() {
        let mix = [vec![0.5; 10], vec![-0.5; 10]];
        let out = separate(
            &mix,
            64,
            2,
            0.25,
            1,
            &mut fake(2, 64),
            &mut |_, _| {},
            &CancelToken::new(),
        )
        .unwrap();
        assert_eq!(out[1][0], vec![0.1; 10]);
    }

    #[test]
    fn cancel_stops() {
        let mix = [vec![0.0; 5000], vec![0.0; 5000]];
        let c = CancelToken::new();
        c.cancel();
        assert!(matches!(
            separate(
                &mix,
                1000,
                4,
                0.25,
                1,
                &mut fake(4, 1000),
                &mut |_, _| {},
                &c
            ),
            Err(Error::Cancelled)
        ));
    }

    #[test]
    fn naming_and_layout() {
        assert_eq!(stem_file_name("My Song", "drums"), "My Song - Drums.wav");
        assert_eq!(stem_file_name("x", "piano"), "x - Piano.wav");
        let m = match_layout(vec![vec![1.0; 5], vec![0.0; 5]], 1, 7);
        assert_eq!(m, vec![vec![0.5, 0.5, 0.5, 0.5, 0.5, 0.0, 0.0]]);
    }

    /// Downloads HT-Demucs (316 MB, license notice accepted for the test) and the
    /// ONNX Runtime, then separates a synthetic 10 s clip.
    #[test]
    #[ignore = "network + 316 MB model"]
    fn demucs_end_to_end() {
        let sr = 48_000;
        let n = sr as usize * 10;
        let mut l = vec![0.0f32; n];
        let beat = sr as usize / 2;
        for b in 0..20 {
            synth::mix_at(&mut l, &synth::kick(sr, 0.3), b * beat);
        }
        for (d, s) in l.iter_mut().zip(synth::tone(220.0, 10.0, sr, &[0.2, 0.1])) {
            *d += s;
        }
        let dir = std::env::temp_dir().join(format!("aitrack-stems-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let input = dir.join("Test Song.wav");
        audio::write_wav(&input, &[l.clone(), l], sr, WavFormat::Float32).unwrap();
        let mm = ModelManager::with_dir(crate::models::test_models_dir());
        mm.accept_license(ids::HTDEMUCS).unwrap();
        let files = split_stems_with(
            &mm,
            &input,
            &dir.join("out"),
            &StemOptions::default(),
            &mut |p| eprintln!("{:?} {:.2} {}", p.stage, p.fraction, p.message),
            &CancelToken::new(),
        )
        .unwrap();
        assert_eq!(files.len(), 4);
        for f in &files {
            let a = audio::load(f).unwrap();
            assert_eq!(a.sample_rate, sr);
            assert_eq!(a.frames(), n);
        }
        assert!(
            files[0]
                .file_name()
                .unwrap()
                .to_string_lossy()
                .ends_with(" - Drums.wav")
        );
    }
}
