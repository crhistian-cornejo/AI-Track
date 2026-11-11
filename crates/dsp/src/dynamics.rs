//! Building blocks for limiters and dynamics: a streaming sliding-window
//! maximum, a box smoother and a 4× true-peak detector.

/// Streaming maximum over the last `window` samples (monotonic wedge, after
/// D. Lemire, *Streaming maximum-minimum filter using no more than three
/// comparisons per element*, 2006). O(1) amortised, allocation in `new`
/// only.
#[derive(Clone, Debug)]
pub struct SlidingMax {
    vals: Vec<f32>,
    idx: Vec<u64>,
    head: usize,
    len: usize,
    window: usize,
    n: u64,
}

impl SlidingMax {
    pub fn new(max_window: usize) -> Self {
        let cap = max_window.max(1) + 1;
        Self { vals: vec![0.0; cap], idx: vec![0; cap], head: 0, len: 0, window: max_window.max(1), n: 0 }
    }

    /// Change the window (≤ the constructed maximum). Clears the history.
    pub fn set_window(&mut self, window: usize) {
        self.window = window.clamp(1, self.vals.len() - 1);
        self.reset();
    }

    pub fn window(&self) -> usize {
        self.window
    }

    pub fn reset(&mut self) {
        self.head = 0;
        self.len = 0;
        self.n = 0;
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let cap = self.vals.len();
        // Pop smaller values from the back.
        while self.len > 0 {
            let back = (self.head + self.len - 1) % cap;
            if self.vals[back] <= x {
                self.len -= 1;
            } else {
                break;
            }
        }
        let pos = (self.head + self.len) % cap;
        self.vals[pos] = x;
        self.idx[pos] = self.n;
        self.len += 1;
        // Expire the front.
        while self.n - self.idx[self.head] >= self.window as u64 {
            self.head = (self.head + 1) % cap;
            self.len -= 1;
        }
        self.n += 1;
        self.vals[self.head]
    }
}

/// Moving average over a fixed window (running sum in `f64`).
#[derive(Clone, Debug)]
pub struct BoxFilter {
    buf: Vec<f32>,
    pos: usize,
    window: usize,
    sum: f64,
}

impl BoxFilter {
    pub fn new(max_window: usize) -> Self {
        Self { buf: vec![0.0; max_window.max(1)], pos: 0, window: max_window.max(1), sum: 0.0 }
    }

    /// Change the window and fill the history with `value`.
    pub fn set_window(&mut self, window: usize, value: f32) {
        self.window = window.clamp(1, self.buf.len());
        self.fill(value);
    }

    pub fn fill(&mut self, value: f32) {
        self.buf[..self.window].fill(value);
        self.sum = value as f64 * self.window as f64;
        self.pos = 0;
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        self.sum += x as f64 - self.buf[self.pos] as f64;
        self.buf[self.pos] = x;
        self.pos += 1;
        if self.pos >= self.window {
            self.pos = 0;
        }
        (self.sum / self.window as f64) as f32
    }
}

/// Taps per polyphase branch of the true-peak interpolator.
const TP_TAPS: usize = 16;
const TP_PHASES: usize = 4;

/// 4× oversampled peak detector (ITU-R BS.1770 Annex 2 method: polyphase
/// interpolation, then absolute peak). Kaiser-windowed sinc, 64 taps.
/// Its output lags the input by [`TruePeak::LATENCY`] samples.
#[derive(Clone, Debug)]
pub struct TruePeak {
    coefs: [[f32; TP_TAPS]; TP_PHASES],
    hist: [f32; TP_TAPS * 2],
    pos: usize,
}

impl Default for TruePeak {
    fn default() -> Self {
        Self::new()
    }
}

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let q = x * x / 4.0;
    for k in 1..50 {
        term *= q / (k * k) as f64;
        sum += term;
        if term < 1e-12 * sum {
            break;
        }
    }
    sum
}

impl TruePeak {
    pub const LATENCY: usize = TP_TAPS / 2;

    pub fn new() -> Self {
        let mut coefs = [[0.0f32; TP_TAPS]; TP_PHASES];
        let beta = 7.0;
        let total = (TP_TAPS * TP_PHASES) as f64;
        let cutoff = 0.92; // of the base-rate Nyquist
        for (p, row) in coefs.iter_mut().enumerate() {
            let mut sum = 0.0;
            for (k, c) in row.iter_mut().enumerate() {
                // Position of this tap in the 4× grid, centred.
                let n = (k * TP_PHASES + p) as f64;
                let t = (n - (total - 1.0) / 2.0) / TP_PHASES as f64;
                let x = std::f64::consts::PI * t * cutoff;
                let sinc = if x.abs() < 1e-12 { 1.0 } else { x.sin() / x };
                let r = (2.0 * n / (total - 1.0)) - 1.0;
                let w = bessel_i0(beta * (1.0 - r * r).max(0.0).sqrt()) / bessel_i0(beta);
                *c = (sinc * w) as f32;
                sum += sinc * w;
            }
            for c in row.iter_mut() {
                *c /= sum as f32;
            }
        }
        Self { coefs, hist: [0.0; TP_TAPS * 2], pos: 0 }
    }

    pub fn reset(&mut self) {
        self.hist = [0.0; TP_TAPS * 2];
        self.pos = 0;
    }

    /// Push a sample; returns the absolute peak of the 4 interpolated
    /// samples around the sample `LATENCY` steps ago.
    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        // Doubled history avoids wrap handling.
        self.hist[self.pos] = x;
        self.hist[self.pos + TP_TAPS] = x;
        self.pos = (self.pos + 1) % TP_TAPS;
        let h = &self.hist[self.pos..self.pos + TP_TAPS];
        let mut peak = 0.0f32;
        for row in &self.coefs {
            let mut acc = 0.0;
            for k in 0..TP_TAPS {
                acc += row[k] * h[TP_TAPS - 1 - k];
            }
            peak = peak.max(acc.abs());
        }
        peak
    }
}

/// True-peak meter: running maximum of [`TruePeak`] with a hold that the
/// caller resets.
#[derive(Clone, Debug, Default)]
pub struct TruePeakMeter {
    tp: TruePeak,
    pub max: f32,
}

impl TruePeakMeter {
    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let v = self.tp.process(x);
        self.max = self.max.max(v);
        v
    }

    pub fn reset(&mut self) {
        self.tp.reset();
        self.max = 0.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sliding_max_matches_brute_force() {
        let mut sm = SlidingMax::new(7);
        let mut noise = crate::osc::Noise::new(3);
        let mut hist = Vec::new();
        for _ in 0..2000 {
            let x = noise.next();
            hist.push(x);
            let got = sm.process(x);
            let start = hist.len().saturating_sub(7);
            let want = hist[start..].iter().cloned().fold(f32::MIN, f32::max);
            assert_eq!(got, want);
        }
    }

    #[test]
    fn true_peak_sees_intersample_overs() {
        // fs/4 sine with 45° phase: samples at ±0.707, true peak 1.0.
        let mut tp = TruePeak::new();
        let mut peak = 0.0f32;
        let mut sample_peak = 0.0f32;
        for n in 0..1000 {
            let x = (std::f32::consts::FRAC_PI_2 * n as f32 + std::f32::consts::FRAC_PI_4).sin();
            sample_peak = sample_peak.max(x.abs());
            if n > 100 {
                peak = peak.max(tp.process(x));
            } else {
                tp.process(x);
            }
        }
        assert!((sample_peak - std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-3);
        assert!(peak > 0.98 && peak < 1.02, "{peak}");
    }

    #[test]
    fn true_peak_is_flat_for_low_tones() {
        let mut tp = TruePeak::new();
        let mut peak = 0.0f32;
        for n in 0..48_000 {
            let x = 0.5 * (std::f32::consts::TAU * 997.0 * n as f32 / 48_000.0).sin();
            peak = peak.max(tp.process(x));
        }
        assert!((peak - 0.5).abs() < 0.005, "{peak}");
    }

    #[test]
    fn box_filter_average() {
        let mut b = BoxFilter::new(4);
        b.fill(1.0);
        assert_eq!(b.process(1.0), 1.0);
        b.process(0.0);
        assert!((b.process(0.0) - 0.5).abs() < 1e-6);
    }
}
