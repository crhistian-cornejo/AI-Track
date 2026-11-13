//! Wave digital filter (WDF) building blocks for the amp/pedal circuits.
//!
//! Voltage waves `a = v + R i`, `b = v − R i` at each port of port
//! resistance `R` (Fettweis 1986). Linear elements discretised with the
//! bilinear transform: a resistor reflects nothing (`b = 0`), a capacitor
//! (`R = T / 2C`) reflects its previous incident wave, a resistive voltage
//! source reflects its voltage. Junctions are adapted three-port parallel
//! adaptors (the upward port reflection-free), and the single nonlinearity
//! sits at the root, solved in closed form with the Wright omega function
//! (Werner, Nangia, Bernardini, Smith & Sarti, DAFx-15 "An improved and
//! generalized diode clipper model for wave digital filters"; D'Angelo,
//! Gabrielli & Turchet, DAFx-19, for fast ω).

/// Wright omega `ω(x)`, the solution `w` of `w + ln w = x` (`ω(x) =
/// W(eˣ)`). A piecewise first guess and up to four Newton steps (stopping once a
/// step is below 1e-12 relative): relative error
/// below 1e-12 for the range the diode models use.
#[inline]
pub fn omega(x: f64) -> f64 {
    if x < -36.0 {
        return x.exp();
    }
    let mut w = if x < -1.0 {
        x.exp()
    } else if x < 1.0 {
        0.567_143 + 0.36 * x
    } else {
        let l = x.ln();
        x - l + l / x
    };
    for _ in 0..4 {
        let f = w + w.ln() - x;
        let step = f * w / (1.0 + w);
        w = (w - step).max(1e-300);
        if step.abs() < 1e-12 * w {
            break;
        }
    }
    w
}

/// A diode (Shockley: `i = Is (e^{v / (n Vt)} − 1)`).
#[derive(Clone, Copy, Debug)]
pub struct Diode {
    /// Saturation current (A).
    pub is: f64,
    /// Ideality times the thermal voltage (V).
    pub nvt: f64,
}

/// Thermal voltage at room temperature (V).
pub const VT: f64 = 0.025_85;

impl Diode {
    /// 1N4148 / 1N914 silicon switching diode.
    pub const SILICON: Diode = Diode { is: 2.52e-9, nvt: 1.752 * VT };
    /// 1N34A germanium diode: lower knee, softer.
    pub const GERMANIUM: Diode = Diode { is: 2.0e-7, nvt: 1.3 * VT };
    /// Red LED: high knee, hard.
    pub const LED: Diode = Diode { is: 9.3e-20, nvt: 1.9 * VT };

    /// Reflected wave of this diode as the root of a port of resistance
    /// `r`, for incident wave `a ≥ 0` (the diode conducting forward):
    /// `b = a + 2 R Is − 2 nVt ω(ln(R Is / nVt) + (a + R Is) / nVt)`.
    #[inline]
    pub fn reflect(&self, a: f64, r: f64) -> f64 {
        let ris = r * self.is;
        a + 2.0 * ris - 2.0 * self.nvt * omega((ris / self.nvt).ln() + (a + ris) / self.nvt)
    }
}

/// Antiparallel diode pair at the root: the forward diode of each polarity
/// conducts and the other one's leakage is neglected (exact up to `Is`).
/// The two sides may differ (asymmetric clipping).
#[derive(Clone, Copy, Debug)]
pub struct DiodePair {
    pub pos: Diode,
    pub neg: Diode,
}

impl DiodePair {
    pub fn symmetric(d: Diode) -> Self {
        Self { pos: d, neg: d }
    }

    #[inline]
    pub fn reflect(&self, a: f64, r: f64) -> f64 {
        if a >= 0.0 { self.pos.reflect(a, r) } else { -self.neg.reflect(-a, r) }
    }
}

/// The RC diode clipper: a voltage source with series resistance `R`, a
/// capacitor `C` and a diode pair, all in parallel. Low-pass at
/// `1 / (2π R C)` for small signals, clipping at the diodes' knee above.
///
/// Tree: root = diode pair; below it a parallel adaptor whose children are
/// the resistive source (port 1) and the capacitor (port 2).
#[derive(Clone, Copy, Debug)]
pub struct DiodeClipper {
    pub diodes: DiodePair,
    /// Adaptor coefficient of the source, `G1 / (G1 + G2)`.
    gamma: f64,
    /// Port resistance seen by the root, `1 / (G1 + G2)`.
    r_root: f64,
    /// Capacitor state (its previous incident wave).
    z: f64,
}

impl DiodeClipper {
    pub fn new(diodes: DiodePair) -> Self {
        let mut c = Self { diodes, gamma: 0.5, r_root: 1.0, z: 0.0 };
        c.set(4_700.0, 47e-9, 96_000.0);
        c
    }

    /// Component values and the (oversampled) rate the clipper runs at.
    /// Keeps the capacitor's charge.
    pub fn set(&mut self, r: f64, c: f64, sample_rate: f64) {
        let rc = 1.0 / (2.0 * c * sample_rate);
        let (g1, g2) = (1.0 / r, 1.0 / rc);
        self.gamma = g1 / (g1 + g2);
        self.r_root = 1.0 / (g1 + g2);
    }

    pub fn reset(&mut self) {
        self.z = 0.0;
    }

    /// Voltage across the diodes for source voltage `vin`.
    #[inline]
    pub fn process(&mut self, vin: f64) -> f64 {
        let (a1, a2) = (vin, self.z);
        let b3 = self.gamma * a1 + (1.0 - self.gamma) * a2;
        let a3 = self.diodes.reflect(b3, self.r_root);
        let v = 0.5 * (a3 + b3);
        // Wave reflected down to the capacitor, `2v − a2`.
        self.z = a3 + b3 - a2;
        if !self.z.is_finite() {
            self.z = 0.0;
            return 0.0;
        }
        if self.z.abs() < 1e-30 {
            self.z = 0.0;
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn omega_solves_its_equation() {
        let mut x = -40.0;
        while x < 60.0 {
            let w = omega(x);
            let err = (w + w.ln() - x).abs();
            assert!(err < 1e-9 * x.abs().max(1.0), "ω({x}) = {w}, residual {err}");
            x += 0.093;
        }
    }

    /// The closed-form diode reflection satisfies Kirchhoff + Shockley.
    #[test]
    fn diode_reflection_satisfies_shockley() {
        for d in [Diode::SILICON, Diode::GERMANIUM, Diode::LED] {
            for r in [100.0, 4_700.0, 100_000.0] {
                for a in [0.0, 0.01, 0.3, 1.0, 5.0, 40.0] {
                    let b = d.reflect(a, r);
                    let (v, i) = (0.5 * (a + b), (a - b) / (2.0 * r));
                    let shockley = d.is * ((v / d.nvt).exp() - 1.0);
                    assert!((i - shockley).abs() <= 1e-6 * i.abs().max(1e-9), "a {a} r {r}: i {i} vs {shockley}");
                }
            }
        }
    }

    #[test]
    fn clipper_is_a_lowpass_that_clips() {
        let sr = 96_000.0;
        let mut c = DiodeClipper::new(DiodePair::symmetric(Diode::SILICON));
        c.set(2_200.0, 10e-9, sr); // fc ≈ 7.2 kHz
        // Small signal at 500 Hz passes nearly untouched…
        let mut peak = 0.0f64;
        for i in 0..9600 {
            let y = c.process(0.01 * (std::f64::consts::TAU * 500.0 * i as f64 / sr).sin());
            if i > 4800 {
                peak = peak.max(y.abs());
            }
        }
        assert!((peak / 0.01 - 1.0).abs() < 0.02, "small-signal gain {}", peak / 0.01);
        // …a big one is held near the knee (~0.6–0.8 V for silicon).
        c.reset();
        let mut peak = 0.0f64;
        for i in 0..9600 {
            peak = peak.max(c.process(10.0 * (std::f64::consts::TAU * 500.0 * i as f64 / sr).sin()).abs());
        }
        assert!(peak > 0.5 && peak < 1.0, "clip level {peak}");
        // Germanium clips lower.
        let mut g = DiodeClipper::new(DiodePair::symmetric(Diode::GERMANIUM));
        g.set(2_200.0, 10e-9, sr);
        let mut gp = 0.0f64;
        for i in 0..9600 {
            gp = gp.max(g.process(10.0 * (std::f64::consts::TAU * 500.0 * i as f64 / sr).sin()).abs());
        }
        assert!(gp < peak, "germanium {gp} vs silicon {peak}");
    }

    #[test]
    fn clipper_survives_garbage() {
        let mut c = DiodeClipper::new(DiodePair::symmetric(Diode::LED));
        for x in [1e12, -1e12, f64::NAN, 0.0, 1e-300, 3.0] {
            let y = c.process(x);
            assert!(y.is_finite() || x.is_nan());
        }
        assert!(c.process(0.1).is_finite());
    }
}
