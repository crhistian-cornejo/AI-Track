//! Band-limited "virtual analogue" oscillators (PolyBLEP / PolyBLAMP) and a
//! fast white-noise source.
//!
//! After V. Välimäki & A. Huovilainen, *Antialiasing Oscillators in
//! Subtractive Synthesis* (IEEE SPM, 2007) and Esqueda, Välimäki & Bilbao,
//! *Rounding corners with BLAMP* (DAFx-16).

/// 2-sample polynomial band-limited step residual.
#[inline]
pub fn poly_blep(t: f32, dt: f32) -> f32 {
    if t < dt {
        let x = t / dt;
        x + x - x * x - 1.0
    } else if t > 1.0 - dt {
        let x = (t - 1.0) / dt;
        x * x + x + x + 1.0
    } else {
        0.0
    }
}

/// 2-sample polynomial band-limited ramp residual.
#[inline]
pub fn poly_blamp(t: f32, dt: f32) -> f32 {
    if t < dt {
        let x = t / dt - 1.0;
        -x * x * x / 3.0
    } else if t > 1.0 - dt {
        let x = (t - 1.0) / dt + 1.0;
        x * x * x / 3.0
    } else {
        0.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VaShape {
    Sine,
    Triangle,
    Saw,
    /// Pulse with variable width (square at 0.5).
    Pulse,
}

/// PolyBLEP oscillator.
#[derive(Clone, Copy, Debug)]
pub struct BlepOsc {
    pub shape: VaShape,
    phase: f32,
    dt: f32,
    pub pulse_width: f32,
}

impl Default for BlepOsc {
    fn default() -> Self {
        Self { shape: VaShape::Saw, phase: 0.0, dt: 0.0, pulse_width: 0.5 }
    }
}

impl BlepOsc {
    pub fn set_freq(&mut self, hz: f32, sample_rate: f32) {
        self.dt = (hz / sample_rate).clamp(0.0, 0.5);
    }

    pub fn set_phase(&mut self, phase: f32) {
        self.phase = phase.rem_euclid(1.0);
    }

    pub fn phase(&self) -> f32 {
        self.phase
    }

    #[inline]
    pub fn next(&mut self) -> f32 {
        let t = self.phase;
        let dt = self.dt;
        let y = match self.shape {
            VaShape::Sine => (t * std::f32::consts::TAU).sin(),
            VaShape::Saw => 2.0 * t - 1.0 - poly_blep(t, dt),
            VaShape::Pulse => {
                let pw = self.pulse_width.clamp(0.02, 0.98);
                let mut y = if t < pw { 1.0 } else { -1.0 };
                y += poly_blep(t, dt);
                y -= poly_blep((t - pw).rem_euclid(1.0), dt);
                // Remove the DC a non-square pulse carries.
                y - (2.0 * pw - 1.0)
            }
            VaShape::Triangle => {
                let mut y = 1.0 - 4.0 * (t - 0.5).abs();
                // Slope changes by ±8 per cycle at t = 0 and t = 0.5.
                y += 4.0 * dt * (poly_blamp(t, dt) - poly_blamp((t + 0.5).rem_euclid(1.0), dt));
                y
            }
        };
        self.phase += dt;
        if self.phase >= 1.0 {
            self.phase -= 1.0;
        }
        y
    }
}

/// xorshift32 white noise in −1..1.
#[derive(Clone, Copy, Debug)]
pub struct Noise {
    state: u32,
}

impl Noise {
    pub fn new(seed: u32) -> Self {
        Self { state: seed.wrapping_mul(747_796_405).wrapping_add(2_891_336_453) | 1 }
    }

    #[inline]
    pub fn next_u32(&mut self) -> u32 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.state = x;
        x
    }

    #[inline]
    pub fn next(&mut self) -> f32 {
        (self.next_u32() >> 8) as f32 * (2.0 / 16_777_216.0) - 1.0
    }

    /// Uniform 0..1.
    #[inline]
    pub fn next_unipolar(&mut self) -> f32 {
        (self.next_u32() >> 8) as f32 * (1.0 / 16_777_216.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oscillators_bounded_and_dc_free() {
        for shape in [VaShape::Sine, VaShape::Triangle, VaShape::Saw, VaShape::Pulse] {
            let mut o = BlepOsc { shape, pulse_width: 0.3, ..Default::default() };
            o.set_freq(441.0, 44_100.0);
            let mut sum = 0.0f64;
            for _ in 0..44_100 {
                let y = o.next();
                assert!(y.abs() <= 1.6 && y.is_finite(), "{shape:?} {y}");
                sum += y as f64;
            }
            assert!((sum / 44_100.0).abs() < 0.01, "{shape:?} dc {}", sum / 44_100.0);
        }
    }

    #[test]
    fn noise_is_uniform_ish() {
        let mut n = Noise::new(7);
        let mut sum = 0.0f64;
        let mut sq = 0.0f64;
        for _ in 0..100_000 {
            let v = n.next() as f64;
            assert!((-1.0..=1.0).contains(&v));
            sum += v;
            sq += v * v;
        }
        assert!((sum / 1e5).abs() < 0.01);
        assert!((sq / 1e5 - 1.0 / 3.0).abs() < 0.01);
    }
}
