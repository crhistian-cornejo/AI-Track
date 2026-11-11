//! Parameter descriptions and lock-free parameter storage.

use std::sync::atomic::{AtomicU32, Ordering};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ParamScale {
    Linear,
    /// Logarithmic (frequencies, times). Requires `min > 0`.
    Log,
    /// Power curve with the given exponent (skewed knobs).
    Power(f32),
    /// Discrete steps (enums, switches).
    Stepped,
}

#[derive(Clone, Debug)]
pub struct ParamInfo {
    pub id: u32,
    pub name: &'static str,
    /// Short name for small knobs.
    pub short: &'static str,
    pub unit: &'static str,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub scale: ParamScale,
    /// Labels for stepped params (index = step).
    pub choices: &'static [&'static str],
    /// Can be automated / mapped to macros.
    pub automatable: bool,
}

impl ParamInfo {
    pub const fn new(id: u32, name: &'static str, min: f32, max: f32, default: f32) -> Self {
        Self { id, name, short: name, unit: "", min, max, default, scale: ParamScale::Linear, choices: &[], automatable: true }
    }
    pub const fn unit(mut self, unit: &'static str) -> Self {
        self.unit = unit;
        self
    }
    pub const fn short(mut self, short: &'static str) -> Self {
        self.short = short;
        self
    }
    pub const fn log(mut self) -> Self {
        self.scale = ParamScale::Log;
        self
    }
    pub const fn power(mut self, exp: f32) -> Self {
        self.scale = ParamScale::Power(exp);
        self
    }
    pub const fn choices(mut self, choices: &'static [&'static str]) -> Self {
        self.scale = ParamScale::Stepped;
        self.choices = choices;
        self
    }

    pub fn to_plain(&self, normalized: f32) -> f32 {
        let n = normalized.clamp(0.0, 1.0);
        match self.scale {
            ParamScale::Linear => self.min + (self.max - self.min) * n,
            ParamScale::Log => self.min * (self.max / self.min).powf(n),
            ParamScale::Power(e) => self.min + (self.max - self.min) * n.powf(e),
            ParamScale::Stepped => {
                let steps = (self.max - self.min).round();
                self.min + (n * steps).round()
            }
        }
    }

    pub fn to_normalized(&self, plain: f32) -> f32 {
        let v = plain.clamp(self.min.min(self.max), self.max.max(self.min));
        let n = match self.scale {
            ParamScale::Linear | ParamScale::Stepped => (v - self.min) / (self.max - self.min),
            ParamScale::Log => (v / self.min).ln() / (self.max / self.min).ln(),
            ParamScale::Power(e) => ((v - self.min) / (self.max - self.min)).powf(1.0 / e),
        };
        if n.is_finite() { n.clamp(0.0, 1.0) } else { 0.0 }
    }

    pub fn default_normalized(&self) -> f32 {
        self.to_normalized(self.default)
    }

    /// Display text such as "1.20 kHz", "-6.0 dB", "Bell".
    pub fn format(&self, normalized: f32) -> String {
        let v = self.to_plain(normalized);
        if self.scale == ParamScale::Stepped
            && let Some(label) = self.choices.get((v - self.min) as usize)
        {
            return (*label).to_string();
        }
        match self.unit {
            "Hz" if v >= 1000.0 => format!("{:.2} kHz", v / 1000.0),
            "Hz" => format!("{v:.0} Hz"),
            "ms" if v >= 1000.0 => format!("{:.2} s", v / 1000.0),
            "ms" => format!("{v:.1} ms"),
            "dB" => format!("{v:+.1} dB"),
            "%" => format!("{v:.0}%"),
            "" => format!("{v:.2}"),
            u => format!("{v:.2} {u}"),
        }
    }
}

/// Lock-free parameter values shared between the UI/engine control thread
/// and the audio thread (f32 stored as bits in atomics).
pub struct ParamValues {
    values: Box<[AtomicU32]>,
}

impl ParamValues {
    pub fn new(params: &[ParamInfo]) -> Self {
        let max_id = params.iter().map(|p| p.id).max().map_or(0, |m| m as usize + 1);
        let values: Box<[AtomicU32]> = (0..max_id).map(|_| AtomicU32::new(0)).collect();
        let pv = Self { values };
        for p in params {
            pv.set(p.id, p.default_normalized());
        }
        pv
    }

    #[inline]
    pub fn get(&self, id: u32) -> f32 {
        self.values.get(id as usize).map_or(0.0, |a| f32::from_bits(a.load(Ordering::Relaxed)))
    }

    #[inline]
    pub fn set(&self, id: u32, normalized: f32) {
        if let Some(a) = self.values.get(id as usize) {
            a.store(normalized.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_scale_round_trip() {
        let p = ParamInfo::new(0, "Freq", 20.0, 20_000.0, 1000.0).unit("Hz").log();
        let n = p.to_normalized(1000.0);
        assert!((p.to_plain(n) - 1000.0).abs() < 0.01);
        assert_eq!(p.format(1.0), "20.00 kHz");
    }

    #[test]
    fn stepped_labels() {
        const TYPES: &[&str] = &["Bell", "Low Shelf", "High Shelf"];
        let p = ParamInfo::new(1, "Type", 0.0, 2.0, 0.0).choices(TYPES);
        assert_eq!(p.format(0.5), "Low Shelf");
    }
}
