//! FFT spectrum analyser for UI display: Hann-windowed STFT with 50 %
//! overlap, mapped to log-spaced bands, with peak-style ballistics
//! (instant rise, fall in dB per second) and an optional display tilt.
//!
//! Push samples from `process` (allocation free); the FFT runs inside
//! `push` every hop. Read the smoothed bands with [`SpectrumAnalyzer::bands`].

use realfft::num_complex::Complex;
use realfft::{RealFftPlanner, RealToComplex};
use std::sync::Arc;

pub struct SpectrumAnalyzer {
    fft: Arc<dyn RealToComplex<f32>>,
    size: usize,
    hop: usize,
    ring: Vec<f32>,
    ring_pos: usize,
    since_hop: usize,
    window: Vec<f32>,
    frame: Vec<f32>,
    spec: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    /// FFT bin range per band (inclusive start, exclusive end) and its
    /// fractional position for sub-bin interpolation.
    band_map: Vec<(usize, usize, f32)>,
    band_freq: Vec<f32>,
    bands: Vec<f32>,
    fall_db: f32,
    tilt_db_oct: f32,
    norm: f32,
    sample_rate: f64,
}

/// What an empty band reads.
pub const FLOOR_DB: f32 = -120.0;

impl SpectrumAnalyzer {
    /// `size` is rounded to a power of two (512..=16384). Bands are
    /// log-spaced from `f_lo` to `f_hi`.
    pub fn new(size: usize, n_bands: usize, f_lo: f32, f_hi: f32, sample_rate: f64) -> Self {
        let size = size.clamp(512, 16_384).next_power_of_two();
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(size);
        let window: Vec<f32> = (0..size).map(|i| 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / size as f32).cos()).collect();
        // Amplitude normalisation: a full-scale sine reads 0 dB.
        let norm = 2.0 / window.iter().sum::<f32>();
        let n_bands = n_bands.max(2);
        let bin_hz = sample_rate as f32 / size as f32;
        let nyq_bin = size / 2;
        let ratio = (f_hi / f_lo).max(1.0001);
        let mut band_map = Vec::with_capacity(n_bands);
        let mut band_freq = Vec::with_capacity(n_bands);
        for i in 0..n_bands {
            let t = i as f32 / (n_bands - 1) as f32;
            let fc = f_lo * ratio.powf(t);
            let half = ratio.powf(0.5 / (n_bands - 1) as f32);
            let lo = ((fc / half) / bin_hz).floor().max(1.0) as usize;
            let hi = (((fc * half) / bin_hz).ceil() as usize).clamp(lo + 1, nyq_bin);
            band_map.push((lo.min(nyq_bin - 1), hi, fc / bin_hz));
            band_freq.push(fc);
        }
        let scratch = fft.make_scratch_vec();
        Self {
            spec: fft.make_output_vec(),
            fft,
            size,
            hop: size / 2,
            ring: vec![0.0; size],
            ring_pos: 0,
            since_hop: 0,
            window,
            frame: vec![0.0; size],
            scratch,
            band_map,
            band_freq,
            bands: vec![FLOOR_DB; n_bands],
            fall_db: 0.0,
            tilt_db_oct: 0.0,
            norm,
            sample_rate,
        }
        .with_fall(24.0)
    }

    fn with_fall(mut self, db_per_s: f32) -> Self {
        self.set_fall_rate(db_per_s);
        self
    }

    /// Fall-back speed of the display in dB per second.
    pub fn set_fall_rate(&mut self, db_per_s: f32) {
        self.fall_db = db_per_s.max(0.0) * self.hop as f32 / self.sample_rate as f32;
    }

    /// Display tilt around 1 kHz (e.g. 4.5 dB/oct to make pink noise flat).
    pub fn set_tilt(&mut self, db_per_oct: f32) {
        self.tilt_db_oct = db_per_oct;
    }

    pub fn bands(&self) -> &[f32] {
        &self.bands
    }

    pub fn band_frequencies(&self) -> &[f32] {
        &self.band_freq
    }

    pub fn size(&self) -> usize {
        self.size
    }

    pub fn reset(&mut self) {
        self.ring.fill(0.0);
        self.bands.fill(FLOOR_DB);
        self.since_hop = 0;
    }

    #[inline]
    pub fn push_sample(&mut self, x: f32) {
        self.ring[self.ring_pos] = x;
        self.ring_pos = (self.ring_pos + 1) & (self.size - 1);
        self.since_hop += 1;
        if self.since_hop >= self.hop {
            self.since_hop = 0;
            self.analyse();
        }
    }

    pub fn push(&mut self, samples: &[f32]) {
        for &x in samples {
            self.push_sample(x);
        }
    }

    fn analyse(&mut self) {
        for i in 0..self.size {
            let v = self.ring[(self.ring_pos + i) & (self.size - 1)];
            self.frame[i] = v * self.window[i];
        }
        if self.fft.process_with_scratch(&mut self.frame, &mut self.spec, &mut self.scratch).is_err() {
            return;
        }
        let norm2 = self.norm * self.norm;
        for (b, &(lo, hi, pos)) in self.band_map.iter().enumerate() {
            let p = if hi - lo <= 1 {
                // Narrower than a bin: interpolate the power between bins.
                let i = (pos.floor() as usize).clamp(1, self.spec.len() - 2);
                let t = pos - i as f32;
                let a = self.spec[i].norm_sqr();
                let c = self.spec[i + 1].norm_sqr();
                a + (c - a) * t.clamp(0.0, 1.0)
            } else {
                self.spec[lo..hi].iter().fold(0.0f32, |m, c| m.max(c.norm_sqr()))
            };
            let mut db = 10.0 * (p * norm2).max(1e-14).log10();
            if self.tilt_db_oct != 0.0 {
                db += self.tilt_db_oct * (self.band_freq[b] / 1000.0).log2();
            }
            let db = db.max(FLOOR_DB);
            let cur = self.bands[b];
            self.bands[b] = if db >= cur { db } else { (cur - self.fall_db).max(db) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sine_reads_its_level_at_its_frequency() {
        let sr = 48_000.0;
        let mut a = SpectrumAnalyzer::new(4096, 96, 20.0, 20_000.0, sr);
        for n in 0..48_000 {
            a.push_sample(0.5 * (std::f32::consts::TAU * 1000.0 * n as f32 / sr as f32).sin());
        }
        let (i, &peak) = a.bands().iter().enumerate().max_by(|x, y| x.1.total_cmp(y.1)).unwrap();
        let f = a.band_frequencies()[i];
        assert!((f / 1000.0 - 1.0).abs() < 0.08, "{f}");
        assert!((peak + 6.02).abs() < 1.5, "{peak}");
        // Far away bands are quiet.
        assert!(a.bands()[0] < -60.0);
    }
}
