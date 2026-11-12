//! The voice filters: a stereo filter with several models, configured at
//! control rate and run per sample. Nothing allocates after `prepare`.
//!
//! Models ([`MODELS`], stable indices):
//! * **SVF** — the original ZDF state-variable filter: low/high/band-pass
//!   (the `Type` parameter) at 12 or 24 dB/oct (`Slope`).
//! * **Ladder** — 4-pole zero-delay-feedback ladder low-pass with a `tanh`
//!   input stage; resonance reaches self-oscillation.
//! * **Notch** — SVF band-reject (`Slope` 24 dB cascades two).
//! * **Comb +/−** — feedback comb tuned to the cutoff (positive feedback:
//!   harmonic peaks; negative: odd harmonics, hollow).
//! * **Formant** — three band-passes on vowel formants; the cutoff knob
//!   sweeps A → E → I → O → U.

use aitrack_dsp::svf::{SvfCoeffs, SvfMode, SvfState};

pub const MODELS: &[&str] = &["SVF", "Ladder", "Notch", "Comb +", "Comb −", "Formant"];
pub const TYPES: &[&str] = &["Low-pass", "High-pass", "Band-pass"];

pub mod model {
    pub const SVF: usize = 0;
    pub const LADDER: usize = 1;
    pub const NOTCH: usize = 2;
    pub const COMB_POS: usize = 3;
    pub const COMB_NEG: usize = 4;
    pub const FORMANT: usize = 5;
}

/// Lowest comb frequency (sets the delay buffer).
const COMB_MIN_HZ: f64 = 20.0;

/// Vowel formants `(Hz, gain)` A, E, I, O, U.
const VOWELS: [[(f32, f32); 3]; 5] = [
    [(800.0, 1.0), (1150.0, 0.63), (2900.0, 0.35)],
    [(400.0, 1.0), (1600.0, 0.4), (2700.0, 0.25)],
    [(270.0, 1.0), (2300.0, 0.3), (3000.0, 0.2)],
    [(450.0, 1.0), (800.0, 0.7), (2830.0, 0.12)],
    [(325.0, 1.0), (700.0, 0.4), (2530.0, 0.1)],
];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FilterSettings {
    pub model: usize,
    /// SVF type: 0 LP, 1 HP, 2 BP.
    pub kind: usize,
    pub slope24: bool,
    pub cutoff: f32,
    /// 0..1.
    pub resonance: f32,
    /// 0..1.
    pub drive: f32,
}

impl Default for FilterSettings {
    fn default() -> Self {
        Self { model: 0, kind: 0, slope24: false, cutoff: 20_000.0, resonance: 0.0, drive: 0.0 }
    }
}

fn svf_mode(kind: usize) -> SvfMode {
    match kind {
        1 => SvfMode::Highpass,
        2 => SvfMode::Bandpass,
        _ => SvfMode::Lowpass,
    }
}

/// The SVF sections a setting runs (shared with UI response plots).
pub fn svf_sections(kind: usize, slope24: bool, cutoff: f32, resonance: f32, sample_rate: f64) -> Vec<SvfCoeffs> {
    let mode = svf_mode(kind);
    let r = resonance.clamp(0.0, 1.0);
    if slope24 {
        vec![SvfCoeffs::new(mode, cutoff, 0.541 * (1.0 + r * 0.2), 0.0, sample_rate), SvfCoeffs::new(mode, cutoff, 1.307 * (20.0f32 / 1.307).powf(r), 0.0, sample_rate)]
    } else {
        let q = std::f32::consts::FRAC_1_SQRT_2 * (20.0f32 / std::f32::consts::FRAC_1_SQRT_2).powf(r);
        vec![SvfCoeffs::new(mode, cutoff, q, 0.0, sample_rate)]
    }
}

/// One stereo voice filter.
#[derive(Clone, Debug)]
pub struct VoiceFilter {
    s: FilterSettings,
    sr: f64,
    svf: [[SvfState; 2]; 3],
    coeffs: [SvfCoeffs; 3],
    sections: usize,
    formant_gain: [f32; 3],
    ladder: [[f32; 4]; 2],
    ladder_g: f32,
    ladder_k: f32,
    comb: [Vec<f32>; 2],
    comb_pos: usize,
    comb_delay: f32,
    comb_fb: f32,
    drive_g: f32,
    makeup: f32,
}

impl VoiceFilter {
    pub fn new(sample_rate: f64) -> Self {
        let mut f = Self {
            s: FilterSettings::default(),
            sr: sample_rate,
            svf: [[SvfState::default(); 2]; 3],
            coeffs: [SvfCoeffs::passthrough(); 3],
            sections: 1,
            formant_gain: [0.0; 3],
            ladder: [[0.0; 4]; 2],
            ladder_g: 0.5,
            ladder_k: 0.0,
            comb: [Vec::new(), Vec::new()],
            comb_pos: 0,
            comb_delay: 100.0,
            comb_fb: 0.0,
            drive_g: 1.0,
            makeup: 1.0,
        };
        f.prepare(sample_rate);
        f
    }

    /// Allocate for a sample rate (control thread).
    pub fn prepare(&mut self, sample_rate: f64) {
        self.sr = sample_rate;
        let len = (sample_rate / COMB_MIN_HZ) as usize + 4;
        for c in &mut self.comb {
            c.clear();
            c.resize(len, 0.0);
        }
        self.reset();
    }

    pub fn reset(&mut self) {
        self.svf = [[SvfState::default(); 2]; 3];
        self.ladder = [[0.0; 4]; 2];
        for c in &mut self.comb {
            c.iter_mut().for_each(|v| *v = 0.0);
        }
        self.comb_pos = 0;
    }

    /// Recompute coefficients (control rate; allocation free).
    pub fn configure(&mut self, s: FilterSettings) {
        self.s = s;
        let sr = self.sr;
        let cutoff = s.cutoff.clamp(20.0, (0.45 * sr as f32).min(20_000.0));
        let r = s.resonance.clamp(0.0, 1.0);
        match s.model {
            model::LADDER => {
                let g = (std::f32::consts::PI * cutoff / sr as f32).tan();
                self.ladder_g = g / (1.0 + g);
                self.ladder_k = 4.2 * r;
            }
            model::NOTCH => {
                let q = 0.5 + 6.0 * r * r;
                self.coeffs[0] = SvfCoeffs::new(SvfMode::Notch, cutoff, q, 0.0, sr);
                self.coeffs[1] = self.coeffs[0];
                self.sections = if s.slope24 { 2 } else { 1 };
            }
            model::COMB_POS | model::COMB_NEG => {
                let max = (self.comb[0].len() - 3) as f32;
                self.comb_delay = (sr as f32 / cutoff).clamp(2.0, max);
                let fb = 0.98 * r.sqrt();
                self.comb_fb = if s.model == model::COMB_NEG { -fb } else { fb };
            }
            model::FORMANT => {
                // Cutoff position (log 20 Hz..20 kHz) picks the vowel.
                let t = ((cutoff / 20.0).ln() / 1000f32.ln()).clamp(0.0, 1.0) * 4.0;
                let a = (t as usize).min(3);
                let u = t - a as f32;
                let q = 4.0 + 16.0 * r;
                for i in 0..3 {
                    let (fa, ga) = VOWELS[a][i];
                    let (fb, gb) = VOWELS[a + 1][i];
                    let f = fa * (fb / fa).powf(u);
                    self.coeffs[i] = SvfCoeffs::new(SvfMode::Bandpass, f.min(0.45 * sr as f32), q, 0.0, sr);
                    self.formant_gain[i] = (ga + (gb - ga) * u) * 1.6;
                }
            }
            _ => {
                // Same sections as `svf_sections`, without its Vec.
                self.sections = if s.slope24 { 2 } else { 1 };
                let mode = svf_mode(s.kind);
                if s.slope24 {
                    self.coeffs[0] = SvfCoeffs::new(mode, cutoff, 0.541 * (1.0 + r * 0.2), 0.0, sr);
                    self.coeffs[1] = SvfCoeffs::new(mode, cutoff, 1.307 * (20.0f32 / 1.307).powf(r), 0.0, sr);
                } else {
                    let q = std::f32::consts::FRAC_1_SQRT_2 * (20.0f32 / std::f32::consts::FRAC_1_SQRT_2).powf(r);
                    self.coeffs[0] = SvfCoeffs::new(mode, cutoff, q, 0.0, sr);
                }
            }
        }
        self.drive_g = 1.0 + 15.0 * s.drive * s.drive;
        self.makeup = self.drive_g.powf(-0.4);
    }

    #[inline]
    pub fn process(&mut self, mut l: f32, mut r: f32) -> (f32, f32) {
        if self.s.drive > 0.001 {
            l = (l * self.drive_g).tanh() * self.makeup;
            r = (r * self.drive_g).tanh() * self.makeup;
        }
        match self.s.model {
            model::LADDER => (self.ladder_tick(0, l), self.ladder_tick(1, r)),
            model::COMB_POS | model::COMB_NEG => self.comb_tick(l, r),
            model::FORMANT => {
                let mut out = [0.0f32; 2];
                for (ch, x) in [l, r].into_iter().enumerate() {
                    for i in 0..3 {
                        out[ch] += self.svf[i][ch].process(&self.coeffs[i], x) * self.formant_gain[i];
                    }
                }
                (out[0], out[1])
            }
            _ => {
                let mut yl = self.svf[0][0].process(&self.coeffs[0], l);
                let mut yr = self.svf[0][1].process(&self.coeffs[0], r);
                if self.sections == 2 {
                    yl = self.svf[1][0].process(&self.coeffs[1], yl);
                    yr = self.svf[1][1].process(&self.coeffs[1], yr);
                }
                (yl, yr)
            }
        }
    }

    #[inline]
    fn ladder_tick(&mut self, ch: usize, x: f32) -> f32 {
        let g = self.ladder_g;
        let k = self.ladder_k;
        let s = &mut self.ladder[ch];
        let h = 1.0 - g;
        let big_s = g * g * g * h * s[0] + g * g * h * s[1] + g * h * s[2] + h * s[3];
        let g4 = g * g * g * g;
        let y4 = (g4 * x + big_s) / (1.0 + k * g4);
        let mut u = (x - k * y4).tanh();
        for st in s.iter_mut() {
            let v = (u - *st) * g;
            let y = v + *st;
            *st = y + v;
            u = y;
        }
        u * (1.0 + 0.3 * k)
    }

    #[inline]
    fn comb_tick(&mut self, l: f32, r: f32) -> (f32, f32) {
        let len = self.comb[0].len();
        let d = self.comb_delay;
        let di = d as usize;
        let t = d - di as f32;
        let read = |buf: &[f32], pos: usize| {
            let a = buf[(pos + len - di) % len];
            let b = buf[(pos + len - di - 1) % len];
            a + (b - a) * t
        };
        let gain = 1.0 - 0.5 * self.comb_fb.abs();
        let mut out = [0.0f32; 2];
        for (ch, x) in [l, r].into_iter().enumerate() {
            let delayed = read(&self.comb[ch], self.comb_pos);
            let y = x + self.comb_fb * delayed;
            let y = if y.is_finite() { y.clamp(-8.0, 8.0) } else { 0.0 };
            self.comb[ch][self.comb_pos] = y;
            out[ch] = y * gain;
        }
        self.comb_pos = (self.comb_pos + 1) % len;
        (out[0], out[1])
    }

    /// Guard against blow-ups (NaN after extreme modulation).
    pub fn sanitize(&mut self) {
        let ok = self.svf.iter().flatten().all(|s| s.is_finite()) && self.ladder.iter().flatten().all(|v| v.is_finite());
        if !ok {
            self.svf = [[SvfState::default(); 2]; 3];
            self.ladder = [[0.0; 4]; 2];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gain_at(f: &mut VoiceFilter, hz: f32) -> f32 {
        f.reset();
        let sr = 48_000.0;
        let n = 9600;
        let mut e = 0.0f32;
        for i in 0..n {
            let x = (std::f32::consts::TAU * hz * i as f32 / sr).sin();
            let (y, _) = f.process(x, x);
            if i > n / 2 {
                e += y * y;
            }
        }
        (e / (n / 2) as f32 * 2.0).sqrt()
    }

    #[test]
    fn models_shape_the_spectrum() {
        let mut f = VoiceFilter::new(48_000.0);
        f.configure(FilterSettings { model: model::LADDER, cutoff: 500.0, resonance: 0.2, ..Default::default() });
        assert!(gain_at(&mut f, 100.0) > 0.5 && gain_at(&mut f, 5000.0) < 0.02);
        f.configure(FilterSettings { model: model::NOTCH, cutoff: 1000.0, resonance: 0.0, ..Default::default() });
        assert!(gain_at(&mut f, 1000.0) < 0.1 && gain_at(&mut f, 100.0) > 0.8);
        f.configure(FilterSettings { model: model::COMB_POS, cutoff: 480.0, resonance: 0.8, ..Default::default() });
        let peak = gain_at(&mut f, 480.0);
        let dip = gain_at(&mut f, 720.0);
        assert!(peak > 2.0 * dip, "{peak} {dip}");
        f.configure(FilterSettings { model: model::COMB_NEG, cutoff: 480.0, resonance: 0.8, ..Default::default() });
        assert!(gain_at(&mut f, 240.0) > 2.0 * gain_at(&mut f, 480.0));
        f.configure(FilterSettings { model: model::FORMANT, cutoff: 20.0, resonance: 0.3, ..Default::default() });
        assert!(gain_at(&mut f, 800.0) > 3.0 * gain_at(&mut f, 5000.0));
        f.configure(FilterSettings { model: model::SVF, kind: 1, slope24: true, cutoff: 2000.0, ..Default::default() });
        assert!(gain_at(&mut f, 100.0) < 0.01 && gain_at(&mut f, 10_000.0) > 0.8);
    }

    #[test]
    fn ladder_self_oscillates_and_stays_finite() {
        let mut f = VoiceFilter::new(48_000.0);
        f.configure(FilterSettings { model: model::LADDER, cutoff: 1000.0, resonance: 1.0, drive: 1.0, ..Default::default() });
        let mut peak = 0.0f32;
        let (y, _) = f.process(1.0, 1.0);
        assert!(y.is_finite());
        for _ in 0..48_000 {
            let (y, _) = f.process(0.0, 0.0);
            assert!(y.is_finite());
            peak = peak.max(y.abs());
        }
        assert!(peak > 0.01, "{peak}");
    }
}
