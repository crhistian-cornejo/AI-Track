//! Tempo (BPM) detection for loops: onset-strength autocorrelation scored
//! over beat multiples, octave-error correction toward 70–180 BPM, then the
//! duration-in-bars and filename heuristics.

use serde::{Deserialize, Serialize};

use crate::audio::resample_channel;
use crate::dsp::{ANALYSIS_RATE, OnsetEnvelope};

pub const PREFERRED_MIN_BPM: f32 = 70.0;
pub const PREFERRED_MAX_BPM: f32 = 180.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BpmSource {
    /// Onset autocorrelation only.
    Signal,
    /// Signal estimate snapped to the loop length in whole bars.
    Duration,
    /// Tempo written in the file name, confirmed or not contradicted by the signal.
    Filename,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BpmEstimate {
    pub bpm: f32,
    pub confidence: f32,
    pub source: BpmSource,
    /// Loop length in 4/4 bars when it matches the tempo.
    pub bars: Option<u32>,
}

/// Options for [`detect_bpm_with`].
#[derive(Clone, Debug, Default)]
pub struct BpmOptions<'a> {
    /// File name (or path) to scan for "128bpm", "_128_", "128 BPM"...
    pub filename: Option<&'a str>,
    /// Use the clip duration as a hint (the material is a trimmed loop).
    pub use_duration: bool,
}

/// Detect the tempo of a mono signal.
pub fn detect_bpm(samples: &[f32], sample_rate: u32) -> Option<BpmEstimate> {
    detect_bpm_with(
        samples,
        sample_rate,
        &BpmOptions {
            filename: None,
            use_duration: true,
        },
    )
}

pub fn detect_bpm_with(
    samples: &[f32],
    sample_rate: u32,
    opts: &BpmOptions,
) -> Option<BpmEstimate> {
    let duration = samples.len() as f32 / sample_rate.max(1) as f32;
    let hint = opts.filename.and_then(bpm_from_filename);
    let signal = if duration >= 1.0 {
        signal_bpm(samples, sample_rate)
    } else {
        None
    };

    let mut est = match (signal, hint) {
        (Some((bpm, conf)), Some(h)) => {
            let agrees = [1.0, 2.0, 0.5, 1.5, 2.0 / 3.0]
                .iter()
                .any(|m| rel(bpm * m, h.bpm) < 0.04);
            if agrees || h.explicit || conf < 0.25 {
                BpmEstimate {
                    bpm: h.bpm,
                    confidence: if agrees { 0.95 } else { 0.7 },
                    source: BpmSource::Filename,
                    bars: None,
                }
            } else {
                BpmEstimate {
                    bpm,
                    confidence: conf,
                    source: BpmSource::Signal,
                    bars: None,
                }
            }
        }
        (Some((bpm, conf)), None) => BpmEstimate {
            bpm,
            confidence: conf,
            source: BpmSource::Signal,
            bars: None,
        },
        (None, Some(h)) => BpmEstimate {
            bpm: h.bpm,
            confidence: if h.explicit { 0.8 } else { 0.5 },
            source: BpmSource::Filename,
            bars: None,
        },
        (None, None) => return None,
    };

    // A trimmed loop lasts a whole number of bars: snap to that tempo when close.
    if duration >= 1.0 {
        let mut best: Option<(f32, u32)> = None;
        for bars in [1u32, 2, 4, 8, 16, 32] {
            let b = 240.0 * bars as f32 / duration;
            if rel(b, est.bpm) < 0.03 && best.is_none_or(|(x, _)| rel(b, est.bpm) < rel(x, est.bpm))
            {
                best = Some((b, bars));
            }
        }
        if let Some((b, bars)) = best {
            est.bars = Some(bars);
            if opts.use_duration && est.source == BpmSource::Signal {
                est.bpm = b;
                est.source = BpmSource::Duration;
                est.confidence = (est.confidence + 0.25).min(1.0);
            } else if est.source == BpmSource::Filename && rel(b, est.bpm) < 0.005 {
                est.confidence = est.confidence.max(0.97);
            }
        }
    }
    // Present tempi with 0.01 precision; snap near-integers (most loops are produced at integer BPM).
    if (est.bpm - est.bpm.round()).abs() < 0.15 {
        est.bpm = est.bpm.round();
    } else {
        est.bpm = (est.bpm * 100.0).round() / 100.0;
    }
    Some(est)
}

fn rel(a: f32, b: f32) -> f32 {
    (a - b).abs() / b.abs().max(1e-6)
}

/// Raw signal tempo and confidence.
fn signal_bpm(samples: &[f32], sample_rate: u32) -> Option<(f32, f32)> {
    let x = if sample_rate == ANALYSIS_RATE {
        samples.to_vec()
    } else {
        resample_channel(samples, sample_rate, ANALYSIS_RATE).ok()?
    };
    let env = OnsetEnvelope::compute(&x, ANALYSIS_RATE, 1024, 128);
    let fps = env.fps() as f32;
    // Light smoothing widens impulse-like peaks so the lag interpolation is stable.
    let v = &env.values;
    let mut o: Vec<f32> = (0..v.len())
        .map(|i| {
            let g = |j: isize| v.get((i as isize + j) as usize).copied().unwrap_or(0.0);
            0.25 * g(-1) + 0.5 * g(0) + 0.25 * g(1)
        })
        .collect();
    let mean = o.iter().sum::<f32>() / o.len().max(1) as f32;
    for s in &mut o {
        *s -= mean;
    }
    let max_lag = (fps * 60.0 / 30.0) as usize * 4 + 2;
    let max_lag = max_lag.min(o.len().saturating_sub(1));
    if max_lag < 20 {
        return None;
    }
    let ac0: f32 = o.iter().map(|a| a * a).sum();
    if ac0 <= 1e-9 {
        return None;
    }
    let ac: Vec<f32> = (0..=max_lag)
        .map(|l| {
            o[..o.len() - l]
                .iter()
                .zip(&o[l..])
                .map(|(a, b)| a * b)
                .sum::<f32>()
                / ac0
        })
        .collect();
    let ac_at = |lag: f32| -> f32 {
        let i = lag.floor() as usize;
        if i + 1 >= ac.len() {
            return 0.0;
        }
        let f = lag - i as f32;
        ac[i] * (1.0 - f) + ac[i + 1] * f
    };
    // Unbiased-ish normalisation: autocorrelation of a finite signal decays with lag.
    let n = o.len() as f32;
    let score = |bpm: f32| -> f32 {
        let period = fps * 60.0 / bpm;
        let mut s = 0.0;
        let mut w = 0.0;
        for k in 1..=4 {
            let lag = period * k as f32;
            if lag as usize + 1 >= ac.len() {
                break;
            }
            let unbias = n / (n - lag).max(1.0);
            s += ac_at(lag) * unbias;
            w += 1.0;
        }
        if w > 0.0 { s / w } else { 0.0 }
    };
    let mut best = (0.0f32, f32::MIN);
    let mut bpm = 50.0f32;
    while bpm <= 240.0 {
        let s = score(bpm);
        if s > best.1 {
            best = (bpm, s);
        }
        bpm += 0.05;
    }
    // Refine around the peak.
    let (mut b, mut s) = best;
    let mut x = b - 0.1;
    while x <= b + 0.1 {
        let sx = score(x);
        if sx > s {
            s = sx;
            b = x;
        }
        x += 0.005;
    }
    let (b0, s0) = (b, s);
    let mut b = b0;
    // Octave errors: fold into the preferred range, and prefer the double tempo
    // when it explains the onsets nearly as well (every onset on a beat).
    while b > PREFERRED_MAX_BPM {
        b /= 2.0;
    }
    while b < PREFERRED_MIN_BPM {
        b *= 2.0;
    }
    if b * 2.0 <= PREFERRED_MAX_BPM + 1.0 && score(b * 2.0) >= 0.85 * score(b) {
        b *= 2.0;
    }
    if (b * 2.0 / 3.0) >= PREFERRED_MIN_BPM && score(b) < 0.5 * score(b * 2.0 / 3.0) {
        b *= 2.0 / 3.0;
    }
    let sb = score(b).max(0.0);
    let conf = (sb.max(s0 * 0.9)).clamp(0.0, 1.0);
    // Contrast against an off-beat lag guards against flat (non-rhythmic) signals.
    let off = score(b * 1.37).max(0.0);
    let contrast = ((sb - off) / sb.max(1e-6)).clamp(0.0, 1.0);
    Some((b, (conf * (0.4 + 0.6 * contrast)).clamp(0.0, 1.0)))
}

/// A tempo written in a file name.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FilenameBpm {
    pub bpm: f32,
    /// Next to the word "bpm" (vs a bare delimited number).
    pub explicit: bool,
}

/// Parse "128bpm", "128 BPM", "bpm128", "_128_", "-128-", "Loop 128 Am".
pub fn bpm_from_filename(name: &str) -> Option<FilenameBpm> {
    let stem = std::path::Path::new(name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(name)
        .to_lowercase();
    let bytes = stem.as_bytes();
    let mut bare: Option<f32> = None;
    let mut i = 0;
    while i < bytes.len() {
        if !bytes[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
            i += 1;
        }
        let num = &stem[start..i];
        let Ok(v) = num.trim_end_matches('.').parse::<f32>() else {
            continue;
        };
        if !(50.0..=250.0).contains(&v) {
            continue;
        }
        let before = stem[..start].trim_end_matches([' ', '_', '-']);
        let after = stem[i..].trim_start_matches([' ', '_', '-']);
        if after.starts_with("bpm") || before.ends_with("bpm") {
            return Some(FilenameBpm {
                bpm: v,
                explicit: true,
            });
        }
        let delim_before = start == 0 || !bytes[start - 1].is_ascii_alphanumeric();
        let delim_after = i == bytes.len() || !bytes[i].is_ascii_alphanumeric();
        // "808", "909" are drum machines, not tempi.
        if delim_before
            && delim_after
            && bare.is_none()
            && !matches!(num, "808" | "909" | "707" | "606")
        {
            bare = Some(v);
        }
    }
    bare.map(|bpm| FilenameBpm {
        bpm,
        explicit: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::synth;

    fn clicks(bpm: f32, secs: f32, sr: u32) -> Vec<f32> {
        let mut x = vec![0.0f32; (secs * sr as f32) as usize];
        let period = 60.0 / bpm;
        let mut t = 0.0;
        let mut k = 0u32;
        while t < secs {
            let at = (t * sr as f32) as usize;
            // Accent the downbeat a bit like a metronome.
            let click: Vec<f32> = synth::noise(400, k)
                .iter()
                .enumerate()
                .map(|(i, n)| {
                    n * (-(i as f32) / 60.0).exp() * if k.is_multiple_of(4) { 1.0 } else { 0.7 }
                })
                .collect();
            synth::mix_at(&mut x, &click, at);
            t += period;
            k += 1;
        }
        x
    }

    fn check(bpm: f32) {
        // 12.3 s is not a whole number of bars, so this tests the signal path alone.
        let sr = 44_100;
        let x = clicks(bpm, 12.3, sr);
        let e = detect_bpm_with(&x, sr, &BpmOptions::default()).unwrap();
        assert!((e.bpm - bpm).abs() < 0.6, "expected {bpm}, got {e:?}");
        assert!(e.confidence > 0.3, "{e:?}");
    }

    #[test]
    fn click_90() {
        check(90.0);
    }

    #[test]
    fn click_128() {
        check(128.0);
    }

    #[test]
    fn click_174() {
        check(174.0);
    }

    #[test]
    fn exact_loop_snaps_to_bars() {
        let sr = 22_050;
        let bpm = 126.0;
        let secs = 240.0 * 4.0 / bpm; // 4 bars
        let x = clicks(bpm, secs, sr);
        let e = detect_bpm(&x, sr).unwrap();
        assert_eq!(e.bpm, 126.0, "{e:?}");
        assert_eq!(e.bars, Some(4));
    }

    #[test]
    fn filename_hints() {
        assert_eq!(
            bpm_from_filename("Deep_House_Loop_124bpm_Am.wav").unwrap(),
            FilenameBpm {
                bpm: 124.0,
                explicit: true
            }
        );
        assert_eq!(bpm_from_filename("drums 140 BPM.wav").unwrap().bpm, 140.0);
        assert_eq!(bpm_from_filename("BPM95_perc.wav").unwrap().bpm, 95.0);
        assert_eq!(
            bpm_from_filename("reggaeton_95_dembow.wav").unwrap(),
            FilenameBpm {
                bpm: 95.0,
                explicit: false
            }
        );
        assert_eq!(bpm_from_filename("808_long_C.wav"), None);
        assert_eq!(bpm_from_filename("kick_01.wav"), None);
    }
}
