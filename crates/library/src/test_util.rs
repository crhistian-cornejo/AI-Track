//! Helpers shared by the unit tests.

use std::path::Path;

/// Writes a 16-bit sine WAV of `secs` seconds at amplitude `amp` (0..1).
pub fn write_wav(path: &Path, rate: u32, channels: u16, secs: f64, amp: f32) {
    let spec = hound::WavSpec { channels, sample_rate: rate, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    let frames = (secs * rate as f64).round() as u64;
    for i in 0..frames {
        let v = (i as f32 * 440.0 * std::f32::consts::TAU / rate as f32).sin() * amp;
        // Make sure the exact peak is reached once.
        let v = if i == 1 { amp } else { v };
        for _ in 0..channels {
            w.write_sample((v * i16::MAX as f32) as i16).unwrap();
        }
    }
    w.finalize().unwrap();
}

/// Writes a mono 16-bit click track: a short decaying burst on every beat at
/// `bpm`, `beats` beats long.
pub fn write_clicks(path: &Path, rate: u32, bpm: f64, beats: u32) {
    let spec = hound::WavSpec { channels: 1, sample_rate: rate, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    let beat = (60.0 / bpm * rate as f64).round() as u64;
    let click = (rate / 100) as u64; // 10 ms
    for i in 0..beat * beats as u64 {
        let t = i % beat;
        let v = if t < click {
            let env = 1.0 - t as f32 / click as f32;
            (t as f32 * 2_000.0 * std::f32::consts::TAU / rate as f32).sin() * env * 0.8
        } else {
            0.0
        };
        w.write_sample((v * i16::MAX as f32) as i16).unwrap();
    }
    w.finalize().unwrap();
}
