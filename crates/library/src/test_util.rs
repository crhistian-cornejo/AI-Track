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
