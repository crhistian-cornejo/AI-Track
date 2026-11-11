//! Parameter smoothing: one-pole (exponential) and linear ramps.
//!
//! Both are real-time safe and allocation free. Use [`Smoother`] for gains
//! and cutoffs that should glide "musically", [`LinearRamp`] when a value
//! must land exactly on its target after a fixed time (crossfades, mutes).

/// One-pole exponential smoother: `y += (target - y) * coeff`.
#[derive(Clone, Copy, Debug)]
pub struct Smoother {
    value: f32,
    target: f32,
    coeff: f32,
}

impl Smoother {
    pub fn new(value: f32) -> Self {
        Self { value, target: value, coeff: 1.0 }
    }

    /// Time constant in milliseconds (63 % of the way after `ms`).
    pub fn set_time(&mut self, ms: f32, sample_rate: f64) {
        self.coeff = one_pole_coeff(ms, sample_rate);
    }

    #[inline]
    pub fn set_target(&mut self, target: f32) {
        self.target = target;
    }

    /// Jump straight to `value` (after `reset` or on the first block).
    pub fn snap(&mut self, value: f32) {
        self.value = value;
        self.target = value;
    }

    pub fn snap_to_target(&mut self) {
        self.value = self.target;
    }

    #[inline]
    pub fn next(&mut self) -> f32 {
        self.value += (self.target - self.value) * self.coeff;
        if (self.target - self.value).abs() <= 1e-4 * self.target.abs().max(1e-4) {
            self.value = self.target;
        }
        self.value
    }

    /// Advance `n` samples at once (control-rate updates).
    #[inline]
    pub fn advance(&mut self, n: usize) -> f32 {
        if self.value != self.target {
            let k = 1.0 - (1.0 - self.coeff).powi(n as i32);
            self.value += (self.target - self.value) * k;
            if (self.target - self.value).abs() <= 1e-4 * self.target.abs().max(1e-4) {
                self.value = self.target;
            }
        }
        self.value
    }

    #[inline]
    pub fn value(&self) -> f32 {
        self.value
    }

    #[inline]
    pub fn target(&self) -> f32 {
        self.target
    }

    #[inline]
    pub fn is_settled(&self) -> bool {
        self.value == self.target
    }
}

/// Coefficient of a one-pole smoother/envelope with time constant `ms`.
#[inline]
pub fn one_pole_coeff(ms: f32, sample_rate: f64) -> f32 {
    if ms <= 0.0 {
        1.0
    } else {
        let n = ms as f64 * 0.001 * sample_rate;
        (1.0 - (-1.0 / n).exp()) as f32
    }
}

/// Linear ramp that reaches its target in exactly `ramp_len` samples.
#[derive(Clone, Copy, Debug)]
pub struct LinearRamp {
    value: f32,
    target: f32,
    step: f32,
    remaining: u32,
    ramp_len: u32,
}

impl LinearRamp {
    pub fn new(value: f32) -> Self {
        Self { value, target: value, step: 0.0, remaining: 0, ramp_len: 1 }
    }

    pub fn set_time(&mut self, ms: f32, sample_rate: f64) {
        self.ramp_len = ((ms as f64 * 0.001 * sample_rate).round() as u32).max(1);
    }

    pub fn set_ramp_samples(&mut self, n: u32) {
        self.ramp_len = n.max(1);
    }

    #[inline]
    pub fn set_target(&mut self, target: f32) {
        if target != self.target {
            self.target = target;
            self.remaining = self.ramp_len;
            self.step = (target - self.value) / self.ramp_len as f32;
        }
    }

    pub fn snap(&mut self, value: f32) {
        self.value = value;
        self.target = value;
        self.remaining = 0;
    }

    #[inline]
    pub fn next(&mut self) -> f32 {
        if self.remaining > 0 {
            self.remaining -= 1;
            self.value = if self.remaining == 0 { self.target } else { self.value + self.step };
        }
        self.value
    }

    #[inline]
    pub fn value(&self) -> f32 {
        self.value
    }

    pub fn is_settled(&self) -> bool {
        self.remaining == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_pole_converges() {
        let mut s = Smoother::new(0.0);
        s.set_time(10.0, 48_000.0);
        s.set_target(1.0);
        // After one time constant ≈ 63 %.
        let mut v = 0.0;
        for _ in 0..480 {
            v = s.next();
        }
        assert!((v - 0.632).abs() < 0.01, "{v}");
        for _ in 0..48_000 {
            s.next();
        }
        assert_eq!(s.value(), 1.0);
    }

    #[test]
    fn advance_matches_per_sample() {
        let mut a = Smoother::new(0.0);
        let mut b = Smoother::new(0.0);
        a.set_time(5.0, 48_000.0);
        b.set_time(5.0, 48_000.0);
        a.set_target(1.0);
        b.set_target(1.0);
        for _ in 0..32 {
            a.next();
        }
        b.advance(32);
        assert!((a.value() - b.value()).abs() < 1e-5);
    }

    #[test]
    fn linear_ramp_lands_exactly() {
        let mut r = LinearRamp::new(0.0);
        r.set_ramp_samples(10);
        r.set_target(1.0);
        for _ in 0..9 {
            assert!(r.next() < 1.0);
        }
        assert_eq!(r.next(), 1.0);
        assert!(r.is_settled());
    }
}
