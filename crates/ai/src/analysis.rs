//! One-call sample analysis for the smart library.

use std::path::Path;

use aitrack_model::{Key, Scale};
use serde::{Deserialize, Serialize};

use crate::Result;
use crate::audio::{AudioData, load, resample_channel};
use crate::classify::{ClassifyInput, SampleCategory, category_from_name, classify};
use crate::dsp::{ANALYSIS_RATE, Yin, median, rms};
use crate::features::{Features, Loudness, features, loudness};
use crate::key::detect_key;
use crate::tempo::{BpmOptions, detect_bpm_with};

/// Detected root of a one-shot (the note to map it to in a sampler).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RootNote {
    /// MIDI note number.
    pub midi: u8,
    /// Note name with octave, C4 = 60.
    pub name: String,
    /// Detuning from the equal-tempered note, cents.
    pub cents: f32,
    pub hz: f32,
    pub confidence: f32,
}

impl RootNote {
    pub fn key(&self) -> Key {
        Key(self.midi % 12)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SampleAnalysis {
    pub duration: f64,
    pub sample_rate: u32,
    pub channels: u16,
    /// (BPM, confidence 0..1), for loops.
    pub bpm: Option<(f32, f32)>,
    /// Loop length in 4/4 bars when it matches the tempo.
    pub bars: Option<u32>,
    /// (scale, confidence 0..1), for tonal material.
    pub key: Option<(Scale, f32)>,
    pub key_candidates: Vec<(Scale, f32)>,
    pub root_note: Option<RootNote>,
    pub is_loop: bool,
    pub category: SampleCategory,
    pub subcategory: Option<String>,
    pub category_confidence: f32,
    pub loudness: Loudness,
    pub features: Features,
}

/// Decode and analyse a file. Folder names take part in the category lookup
/// ("Kicks/001.wav" is a kick).
pub fn analyze(path: impl AsRef<Path>) -> Result<SampleAnalysis> {
    let path = path.as_ref();
    let audio = load(path)?;
    Ok(analyze_audio(&audio, &name_for(path)))
}

/// File stem plus up to two parent folder names, most specific first.
pub(crate) fn name_for(path: &Path) -> String {
    let mut parts = vec![
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_string(),
    ];
    let mut p = path.parent();
    for _ in 0..2 {
        if let Some(dir) = p {
            if let Some(n) = dir.file_name().and_then(|s| s.to_str()) {
                parts.push(n.to_string());
            }
            p = dir.parent();
        }
    }
    parts.join(" / ")
}

/// Analyse decoded audio. `name` is the file name (and folders) used for keyword hints.
pub fn analyze_audio(audio: &AudioData, name: &str) -> SampleAnalysis {
    let duration = audio.duration();
    let mono_native = audio.to_mono();
    let x = resample_channel(&mono_native, audio.sample_rate, ANALYSIS_RATE).unwrap_or_default();
    let sr = ANALYSIS_RATE;
    let feats = features(&x, sr);
    let loud = loudness(&audio.channels, audio.sample_rate);
    let file_name = name.split(" / ").next().unwrap_or(name);
    let (_, name_says_loop) = category_from_name(name);

    let bpm = detect_bpm_with(
        &x,
        sr,
        &BpmOptions {
            filename: Some(file_name),
            use_duration: true,
        },
    );
    let whole_bars = bpm.as_ref().is_some_and(|b| b.bars.is_some());
    let rhythmic = bpm.as_ref().is_some_and(|b| b.confidence > 0.35) && feats.transients >= 4;
    let is_loop =
        duration >= 1.0 && (name_says_loop || (rhythmic && (whole_bars || duration >= 4.0)));

    let pitch = pitch_summary(&x, sr, &feats);
    let input = ClassifyInput {
        features: &feats,
        duration: duration as f32,
        pitch: pitch.map(|p| (p.0, p.1)),
        is_loop,
        early_bursts: early_bursts(&x, sr),
    };
    let class = classify(name, &input);

    let tonal_material = class.category.is_tonal() || class.category == SampleCategory::Unknown;
    let (key, key_candidates) = if tonal_material && duration >= 0.75 {
        match detect_key(&x, sr) {
            Some(k) if k.confidence >= 0.15 => (Some((k.scale, k.confidence)), k.candidates),
            Some(k) => (None, k.candidates),
            None => (None, Vec::new()),
        }
    } else {
        (None, Vec::new())
    };

    let root_note = if !is_loop
        && !matches!(
            class.category,
            SampleCategory::HiHat
                | SampleCategory::Cymbal
                | SampleCategory::Snare
                | SampleCategory::Clap
        ) {
        pitch.filter(|p| p.1 > 0.5).map(|(hz, clarity)| {
            let m = crate::dsp::hz_to_midi(hz);
            let midi = m.round().clamp(0.0, 127.0) as u8;
            RootNote {
                midi,
                name: note_name(midi),
                cents: (m - midi as f32) * 100.0,
                hz,
                confidence: clarity,
            }
        })
    } else {
        None
    };

    SampleAnalysis {
        duration,
        sample_rate: audio.sample_rate,
        channels: audio.channel_count() as u16,
        bpm: if is_loop {
            bpm.as_ref().map(|b| (b.bpm, b.confidence))
        } else {
            None
        },
        bars: if is_loop {
            bpm.as_ref().and_then(|b| b.bars)
        } else {
            None
        },
        key,
        key_candidates,
        root_note,
        is_loop,
        category: class.category,
        subcategory: class.subcategory,
        category_confidence: class.confidence,
        loudness: loud,
        features: feats,
    }
}

pub fn note_name(midi: u8) -> String {
    format!("{}{}", Key(midi % 12).name(), midi as i32 / 12 - 1)
}

/// Median f0 and clarity (1 - aperiodicity) over the body of the sound.
fn pitch_summary(x: &[f32], sr: u32, f: &Features) -> Option<(f32, f32)> {
    let yin = Yin::new(sr, 30.0, 2000.0, 1024);
    let hop = 256;
    let start = ((f.attack_s + 0.03) * sr as f32) as usize;
    let end = x.len().min(start + (1.5 * sr as f32) as usize);
    if end <= start + yin.frame_len() {
        return None;
    }
    let level = rms(&x[start..end]);
    let mut hz = Vec::new();
    let mut ap = Vec::new();
    let mut i = start;
    while i + yin.frame_len() <= end {
        let frame = &x[i..i + yin.frame_len()];
        if rms(frame) > level * 0.2
            && let Some(p) = yin.estimate(frame)
        {
            ap.push(p.aperiodicity);
            if p.aperiodicity < 0.3 {
                hz.push(p.hz);
            }
        }
        i += hop;
    }
    if ap.is_empty() {
        return None;
    }
    let clarity = 1.0 - median(&mut ap).clamp(0.0, 1.0);
    if hz.len() < 3 {
        return Some((0.0, clarity.min(0.3)));
    }
    let voiced_ratio = hz.len() as f32 / ap.len() as f32;
    Some((median(&mut hz), clarity * voiced_ratio.sqrt()))
}

/// Count separate amplitude bursts in the first 60 ms (claps are 3–4 quick hits).
fn early_bursts(x: &[f32], sr: u32) -> u32 {
    let w = (sr as usize / 1000).max(1);
    let n = x.len().min((0.06 * sr as f32) as usize);
    let env: Vec<f32> = x[..n].chunks(w).map(rms).collect();
    let max = env.iter().copied().fold(0.0f32, f32::max);
    if max <= 0.0 {
        return 0;
    }
    let mut count = 0;
    let mut low = env.first().copied().unwrap_or(0.0);
    let mut armed = true;
    for &v in &env {
        if armed && v > 0.5 * max && v > 2.0 * low.max(1e-6) {
            count += 1;
            armed = false;
            low = v;
        } else if !armed && v < 0.6 * low {
            armed = true;
            low = v;
        } else if armed {
            low = low.min(v);
        } else {
            low = low.max(v);
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dsp::synth;

    fn mono(x: Vec<f32>, sr: u32) -> AudioData {
        AudioData::mono_signal(x, sr)
    }

    #[test]
    fn synthetic_kick_is_kick() {
        let sr = 44_100;
        let a = analyze_audio(&mono(synth::kick(sr, 0.6), sr), "sample_01");
        assert_eq!(a.category, SampleCategory::Kick, "{a:#?}");
        assert!(!a.is_loop);
        assert!(a.bpm.is_none());
    }

    #[test]
    fn synthetic_hat_is_hihat() {
        let sr = 44_100;
        let a = analyze_audio(&mono(synth::hat(sr, 0.15, 7), sr), "sample_02");
        assert_eq!(a.category, SampleCategory::HiHat, "{a:#?}");
        assert_eq!(a.subcategory.as_deref(), Some("closed"));
        assert!(a.key.is_none());
    }

    #[test]
    fn filename_beats_features_and_folders_count() {
        let sr = 44_100;
        let a = analyze_audio(&mono(synth::kick(sr, 0.5), sr), "bombo_duro / Drums");
        assert_eq!(a.category, SampleCategory::Kick);
        assert!(a.category_confidence > 0.9);
        assert_eq!(
            name_for(Path::new("/lib/Kicks/Hard/001.wav")),
            "001 / Hard / Kicks"
        );
    }

    #[test]
    fn bass_one_shot_has_root_note() {
        let sr = 44_100;
        // A1 = 55 Hz with a little harmonic content and a sustained envelope.
        let x: Vec<f32> = synth::tone(55.0, 1.2, sr, &[1.0, 0.3])
            .into_iter()
            .enumerate()
            .map(|(i, s)| s * 0.6 * (-(i as f32) / sr as f32 * 1.5).exp())
            .collect();
        let a = analyze_audio(&mono(x, sr), "untitled");
        let r = a.root_note.clone().expect("root");
        assert_eq!(r.midi, 33, "{a:#?}");
        assert_eq!(r.name, "A1");
        assert!(
            matches!(a.category, SampleCategory::Bass808 | SampleCategory::Bass),
            "{a:#?}"
        );
    }

    #[test]
    fn drum_loop_is_loop_with_bpm() {
        let sr = 44_100;
        let bpm = 128.0f32;
        let beat = 60.0 / bpm;
        let secs = beat * 16.0; // 4 bars
        let mut x = vec![0.0f32; (secs * sr as f32) as usize];
        for b in 0..16 {
            let at = (b as f32 * beat * sr as f32) as usize;
            synth::mix_at(&mut x, &synth::kick(sr, 0.3), at);
            synth::mix_at(
                &mut x,
                &synth::hat(sr, 0.05, b),
                at + (beat * 0.5 * sr as f32) as usize,
            );
        }
        let a = analyze_audio(&mono(x, sr), "groove_a");
        assert!(a.is_loop, "{a:#?}");
        assert_eq!(a.bpm.map(|b| b.0), Some(128.0), "{a:#?}");
        assert_eq!(a.bars, Some(4));
        assert_eq!(a.category, SampleCategory::DrumLoop, "{a:#?}");
    }

    #[test]
    fn serialises() {
        let sr = 22_050;
        let a = analyze_audio(&mono(synth::kick(sr, 0.3), sr), "kick");
        let json = serde_json::to_string(&a).unwrap();
        let back: SampleAnalysis = serde_json::from_str(&json).unwrap();
        assert_eq!(back.category, a.category);
    }
}
