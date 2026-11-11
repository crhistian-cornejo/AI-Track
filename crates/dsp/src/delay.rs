//! Fractional delay line (power-of-two ring) with linear, cubic Hermite and
//! first-order all-pass interpolation, plus a Schroeder all-pass diffuser.
//!
//! Buffers are allocated in [`DelayLine::new`] / [`DelayLine::resize`] only.

/// Ring buffer delay. Push one sample, then read any delay `>= 0` up to
/// `max_delay()`; a delay of 0 returns the sample just pushed.
#[derive(Clone, Debug)]
pub struct DelayLine {
    buf: Vec<f32>,
    mask: usize,
    write: usize,
}

impl DelayLine {
    /// Room for at least `max_delay` samples plus the interpolators' guard.
    pub fn new(max_delay: usize) -> Self {
        let size = (max_delay + 4).next_power_of_two();
        Self { buf: vec![0.0; size], mask: size - 1, write: 0 }
    }

    /// Allocates: call from `prepare`.
    pub fn resize(&mut self, max_delay: usize) {
        *self = Self::new(max_delay);
    }

    /// Longest delay a read may ask for.
    pub fn max_delay(&self) -> usize {
        self.buf.len() - 4
    }

    pub fn clear(&mut self) {
        self.buf.fill(0.0);
    }

    #[inline]
    pub fn push(&mut self, x: f32) {
        self.write = (self.write + 1) & self.mask;
        self.buf[self.write] = x;
    }

    /// Integer delay.
    #[inline]
    pub fn read(&self, delay: usize) -> f32 {
        self.buf[(self.write.wrapping_sub(delay)) & self.mask]
    }

    #[inline]
    fn clamp_delay(&self, delay: f32) -> f32 {
        delay.clamp(0.0, self.max_delay() as f32)
    }

    #[inline]
    pub fn read_linear(&self, delay: f32) -> f32 {
        let d = self.clamp_delay(delay);
        let i = d as usize;
        let frac = d - i as f32;
        let a = self.read(i);
        let b = self.read(i + 1);
        a + (b - a) * frac
    }

    /// 4-point, 3rd-order Hermite (Catmull-Rom) interpolation. Needs
    /// `delay >= 1` for the full kernel; smaller delays clamp to 1.
    #[inline]
    pub fn read_cubic(&self, delay: f32) -> f32 {
        let d = self.clamp_delay(delay).max(1.0);
        let i = d as usize;
        let t = d - i as f32;
        let xm1 = self.read(i - 1);
        let x0 = self.read(i);
        let x1 = self.read(i + 1);
        let x2 = self.read(i + 2);
        hermite(t, xm1, x0, x1, x2)
    }

    /// Write position; useful for tap-tempo and block reads.
    pub fn write_index(&self) -> usize {
        self.write
    }
}

/// 4-point Hermite interpolation between `x0` and `x1` at `t ∈ [0, 1)`.
#[inline]
pub fn hermite(t: f32, xm1: f32, x0: f32, x1: f32, x2: f32) -> f32 {
    let c0 = x0;
    let c1 = 0.5 * (x1 - xm1);
    let c2 = xm1 - 2.5 * x0 + 2.0 * x1 - 0.5 * x2;
    let c3 = 0.5 * (x2 - xm1) + 1.5 * (x0 - x1);
    ((c3 * t + c2) * t + c1) * t + c0
}

/// First-order all-pass (Thiran) fractional reader. Flat magnitude, best for
/// fixed or slowly varying delays (comb/feedback paths); keep one per tap.
#[derive(Clone, Copy, Debug, Default)]
pub struct AllpassReader {
    prev_in: f32,
    prev_out: f32,
}

impl AllpassReader {
    #[inline]
    pub fn read(&mut self, line: &DelayLine, delay: f32) -> f32 {
        // Keep the fractional part in [0.5, 1.5) where the coefficient is
        // well behaved.
        let d = line.clamp_delay(delay).max(0.5);
        let mut i = d.floor() as usize;
        let mut frac = d - i as f32;
        if frac < 0.5 && i > 0 {
            i -= 1;
            frac += 1.0;
        }
        let eta = (1.0 - frac) / (1.0 + frac);
        let x = line.read(i);
        let y = eta * (x - self.prev_out) + self.prev_in;
        self.prev_in = x;
        self.prev_out = if y.abs() < 1e-20 { 0.0 } else { y };
        y
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Schroeder all-pass diffuser `y = -g x + d`, `d ← x + g y` with an
/// integer delay.
#[derive(Clone, Debug)]
pub struct AllpassDiffuser {
    line: DelayLine,
    delay: usize,
}

impl AllpassDiffuser {
    pub fn new(delay: usize) -> Self {
        Self { line: DelayLine::new(delay.max(1)), delay: delay.max(1) }
    }

    pub fn set_delay(&mut self, delay: usize) {
        self.delay = delay.clamp(1, self.line.max_delay());
    }

    #[inline]
    pub fn process(&mut self, x: f32, g: f32) -> f32 {
        let d = self.line.read(self.delay - 1);
        let v = x + g * d;
        self.line.push(if v.abs() < 1e-20 { 0.0 } else { v });
        d - g * v
    }

    /// Fractional, modulated variant (reads with linear interpolation).
    #[inline]
    pub fn process_mod(&mut self, x: f32, g: f32, delay: f32) -> f32 {
        let d = self.line.read_linear((delay - 1.0).max(0.0));
        let v = x + g * d;
        self.line.push(if v.abs() < 1e-20 { 0.0 } else { v });
        d - g * v
    }

    /// Read an internal tap (Dattorro-style output taps).
    #[inline]
    pub fn tap(&self, delay: usize) -> f32 {
        self.line.read(delay)
    }

    pub fn clear(&mut self) {
        self.line.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integer_and_fractional_reads() {
        let mut d = DelayLine::new(16);
        for i in 0..10 {
            d.push(i as f32);
        }
        assert_eq!(d.read(0), 9.0);
        assert_eq!(d.read(3), 6.0);
        assert!((d.read_linear(2.5) - 6.5).abs() < 1e-6);
        assert!((d.read_cubic(2.5) - 6.5).abs() < 1e-5); // exact on a ramp
    }

    #[test]
    fn allpass_reader_delays_a_sine() {
        let sr = 48_000.0f32;
        let mut d = DelayLine::new(64);
        let mut r = AllpassReader::default();
        let f = 500.0;
        let delay = 10.3;
        let mut err = 0.0f32;
        for n in 0..4000 {
            let x = (std::f32::consts::TAU * f * n as f32 / sr).sin();
            d.push(x);
            let y = r.read(&d, delay);
            if n > 1000 {
                let expect = (std::f32::consts::TAU * f * (n as f32 - delay) / sr).sin();
                err = err.max((y - expect).abs());
            }
        }
        assert!(err < 0.01, "{err}");
    }

    #[test]
    fn diffuser_is_allpass_energy_preserving() {
        let mut ap = AllpassDiffuser::new(37);
        let mut e_out = 0.0;
        for n in 0..20_000 {
            let x = if n == 0 { 1.0 } else { 0.0 };
            let y = ap.process(x, 0.6);
            e_out += y * y;
        }
        assert!((e_out - 1.0f32).abs() < 1e-3, "{e_out}");
    }
}
