//! Analogue prototypes of the `fx.eq` band shapes as complex responses
//! `H(jΩ)`, the reference the Natural and Linear phase modes are designed
//! from (and the curve those modes draw).
//!
//! The second-order shapes are the RBJ cookbook prototypes (the same ones the
//! matched biquads fit, see `aitrack_dsp::biquad::analog_mag2`); cuts are
//! Butterworth cascades with the real pole of odd orders as a first-order
//! section; the tilt is a first-order low/high split around the pivot. Pure
//! maths: nothing here touches the audio thread except through the FIR
//! design, which calls it with preallocated buffers.

use aitrack_dsp::response::{Complex, cmul};

#[inline]
fn cdiv(a: Complex, b: Complex) -> Complex {
    let d = (b.0 * b.0 + b.1 * b.1).max(1e-300);
    ((a.0 * b.0 + a.1 * b.1) / d, (a.1 * b.0 - a.0 * b.1) / d)
}

/// `c s² + b s + a` at `s = j x`.
#[inline]
fn quad(c: f64, b: f64, a: f64, x: f64) -> Complex {
    (a - c * x * x, b * x)
}

/// Butterworth Q of 2nd-order section `k` of an order-`n` cascade (odd
/// orders put their complex pairs at `θ = π (k + 1) / n`).
pub fn bw_q(n: usize, k: usize) -> f64 {
    if n.is_multiple_of(2) {
        let theta = std::f64::consts::PI * (2 * k + 1) as f64 / (2 * n) as f64;
        1.0 / (2.0 * theta.cos())
    } else {
        let theta = std::f64::consts::PI * (k + 1) as f64 / n as f64;
        1.0 / (2.0 * theta.cos())
    }
}

/// Complex response of band type `ty` (the `fx.eq` `TYPE` index) at `freq`.
/// `order` is the cut slope in poles (6 dB/oct each).
pub fn band_response(ty: usize, freq: f64, f0: f64, gain_db: f64, q: f64, order: usize) -> Complex {
    Proto::new(ty, f0, gain_db, q, order).at(freq)
}

/// Direct (unprecomputed) evaluation, kept as the reference for [`Proto`].
#[cfg(test)]
fn band_response_direct(ty: usize, freq: f64, f0: f64, gain_db: f64, q: f64, order: usize) -> Complex {
    let x = (freq / f0.max(1e-3)).max(0.0);
    let q = q.max(0.025);
    let a = 10f64.powf(gain_db / 40.0);
    match ty {
        // Bell: (s² + s A/Q + 1) / (s² + s/(A Q) + 1).
        0 => cdiv(quad(1.0, a / q, 1.0, x), quad(1.0, 1.0 / (a * q), 1.0, x)),
        // Low shelf: A (s² + √A/Q s + A) / (A s² + √A/Q s + 1).
        1 => {
            let h = cdiv(quad(1.0, a.sqrt() / q, a, x), quad(a, a.sqrt() / q, 1.0, x));
            (a * h.0, a * h.1)
        }
        // High shelf: A (A s² + √A/Q s + 1) / (s² + √A/Q s + A).
        2 => {
            let h = cdiv(quad(a, a.sqrt() / q, 1.0, x), quad(1.0, a.sqrt() / q, a, x));
            (a * h.0, a * h.1)
        }
        3 | 4 => {
            let high_cut = ty == 4;
            let n = order.max(1);
            let mut acc: Complex = (1.0, 0.0);
            for k in 0..n / 2 {
                let den = quad(1.0, 1.0 / bw_q(n, k), 1.0, x);
                let num = if high_cut { (1.0, 0.0) } else { (-x * x, 0.0) };
                acc = cmul(acc, cdiv(num, den));
            }
            if n % 2 == 1 {
                let den = (1.0, x);
                let num = if high_cut { (1.0, 0.0) } else { (0.0, x) };
                acc = cmul(acc, cdiv(num, den));
            }
            acc
        }
        // Notch: (s² + 1) / (s² + s/Q + 1).
        5 => cdiv(quad(1.0, 0.0, 1.0, x), quad(1.0, 1.0 / q, 1.0, x)),
        // Band-pass, unity peak: (s/Q) / (s² + s/Q + 1).
        6 => cdiv((0.0, x / q), quad(1.0, 1.0 / q, 1.0, x)),
        // Tilt: g_l / (s + 1) + g_h s / (s + 1), ±half the gain either side.
        _ => {
            let gl = 10f64.powf(-gain_db / 40.0);
            let gh = 10f64.powf(gain_db / 40.0);
            cdiv((gl, gh * x), (1.0, x))
        }
    }
}

/// One band's prototype with its per-band constants precomputed, for
/// evaluating many frequencies (the FIR design samples thousands of bins).
#[derive(Clone, Copy, Debug)]
pub struct Proto {
    ty: usize,
    inv_f0: f64,
    a: f64,
    sqrt_a: f64,
    inv_q: f64,
    order: usize,
    inv_bw_q: [f64; 8],
    gl: f64,
    gh: f64,
}

impl Proto {
    pub fn new(ty: usize, f0: f64, gain_db: f64, q: f64, order: usize) -> Self {
        let n = order.clamp(1, 16);
        let mut inv_bw_q = [0.0; 8];
        for (k, v) in inv_bw_q.iter_mut().enumerate().take(n / 2) {
            *v = 1.0 / bw_q(n, k);
        }
        let a = 10f64.powf(gain_db / 40.0);
        Self { ty, inv_f0: 1.0 / f0.max(1e-3), a, sqrt_a: a.sqrt(), inv_q: 1.0 / q.max(0.025), order: n, inv_bw_q, gl: 1.0 / a, gh: a }
    }

    /// `H(j 2π freq)`.
    #[inline]
    pub fn at(&self, freq: f64) -> Complex {
        let x = (freq * self.inv_f0).max(0.0);
        let (a, iq) = (self.a, self.inv_q);
        match self.ty {
            0 => cdiv(quad(1.0, a * iq, 1.0, x), quad(1.0, iq / a, 1.0, x)),
            1 => {
                let h = cdiv(quad(1.0, self.sqrt_a * iq, a, x), quad(a, self.sqrt_a * iq, 1.0, x));
                (a * h.0, a * h.1)
            }
            2 => {
                let h = cdiv(quad(a, self.sqrt_a * iq, 1.0, x), quad(1.0, self.sqrt_a * iq, a, x));
                (a * h.0, a * h.1)
            }
            3 | 4 => {
                let high_cut = self.ty == 4;
                let mut acc: Complex = (1.0, 0.0);
                let num2 = if high_cut { (1.0, 0.0) } else { (-x * x, 0.0) };
                for k in 0..self.order / 2 {
                    acc = cmul(acc, cdiv(num2, quad(1.0, self.inv_bw_q[k], 1.0, x)));
                }
                if self.order % 2 == 1 {
                    let num = if high_cut { (1.0, 0.0) } else { (0.0, x) };
                    acc = cmul(acc, cdiv(num, (1.0, x)));
                }
                acc
            }
            5 => cdiv(quad(1.0, 0.0, 1.0, x), quad(1.0, iq, 1.0, x)),
            6 => cdiv((0.0, x * iq), quad(1.0, iq, 1.0, x)),
            _ => cdiv((self.gl, self.gh * x), (1.0, x)),
        }
    }
}

/// Magnitude of [`band_response`] in dB.
pub fn band_db(ty: usize, freq: f64, f0: f64, gain_db: f64, q: f64, order: usize) -> f64 {
    let h = band_response(ty, freq, f0, gain_db, q, order);
    10.0 * (h.0 * h.0 + h.1 * h.1).max(1e-30).log10()
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitrack_dsp::biquad::{BiquadShape, analog_magnitude_db};

    #[test]
    fn second_order_shapes_match_the_cookbook_magnitudes() {
        let shapes = [(0usize, BiquadShape::Bell), (1, BiquadShape::LowShelf), (2, BiquadShape::HighShelf), (5, BiquadShape::Notch), (6, BiquadShape::Bandpass)];
        for (ty, shape) in shapes {
            for &(f0, g, q) in &[(1000.0, 6.0, 1.0), (200.0, -9.0, 0.707), (8000.0, 4.5, 2.0)] {
                for f in [30.0, 180.0, 1000.0, 5000.0, 15000.0] {
                    let a = band_db(ty, f, f0, g, q, 2);
                    let b = analog_magnitude_db(shape, f, f0, q, g);
                    assert!((a - b).abs() < 1e-6, "type {ty} f0 {f0} at {f}: {a} vs {b}");
                }
            }
        }
    }

    #[test]
    fn precomputed_prototypes_agree_with_the_direct_formulas() {
        for ty in 0..8 {
            for order in [1usize, 2, 3, 8, 16] {
                for f in [20.0, 440.0, 3000.0, 21000.0] {
                    let a = band_response(ty, f, 900.0, -7.5, 1.3, order);
                    let b = band_response_direct(ty, f, 900.0, -7.5, 1.3, order);
                    assert!((a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9, "type {ty} order {order} at {f}");
                }
            }
        }
    }

    #[test]
    fn cuts_are_butterworth() {
        for n in [1usize, 2, 3, 4, 6, 8, 12, 16] {
            // −3 dB at the corner, n·6 dB/oct far away.
            assert!((band_db(3, 100.0, 100.0, 0.0, 0.7, n) + 3.0103).abs() < 1e-3, "order {n}");
            assert!((band_db(4, 100.0, 100.0, 0.0, 0.7, n) + 3.0103).abs() < 1e-3, "order {n}");
            let far = band_db(4, 400.0, 100.0, 0.0, 0.7, n);
            assert!((far + 10.0 * (1.0 + 4f64.powi(2 * n as i32)).log10()).abs() < 1e-3, "order {n}: {far}");
        }
    }

    #[test]
    fn tilt_pivots_around_its_frequency() {
        assert!((band_db(7, 10.0, 1000.0, 6.0, 0.7, 2) + 3.0).abs() < 0.01);
        assert!((band_db(7, 100_000.0, 1000.0, 6.0, 0.7, 2) - 3.0).abs() < 0.01);
    }
}
