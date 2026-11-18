//! Band-limited resampling: one interpolator for everything that reads audio
//! at another speed (the warp's transposition, Re-Pitch clips, the sampler
//! and drum voices, delay-line pitch shifters).
//!
//! **Why.** Reading a signal `r` times faster than it was recorded maps a
//! component at `f` to `r·f`; whatever lands past Nyquist folds back into
//! the band (a 13 kHz sine an octave up comes out at 22 kHz instead of
//! vanishing). A cubic Hermite or a fixed-cutoff sinc cannot stop that: the
//! source has to be low-passed at the *new* Nyquist before it is read.
//!
//! **How.** A Kaiser-windowed sinc evaluated from a table, whose time scale
//! follows the cutoff `fc` (a fraction of the source Nyquist). For a ratio
//! `r > 1` the cutoff drops to about `1/r` and the kernel widens by the same
//! factor (`2·Z/fc` taps for `Z` zero crossings each side), as in Smith's
//! *Digital Audio Resampling* (https://ccrma.stanford.edu/~jos/resample/);
//! for `r ≤ 1` it is a plain band-limited interpolator at the full band. The
//! weights are normalised by their sum, so DC is exact at any cutoff, and at
//! `fc = 1` a whole-sample position reads the stored sample untouched (every
//! other tap sits on a zero of the sinc): unity speed stays bit-exact.
//!
//! **Where the stop band starts.** The cutoff is set so that the kernel's
//! stop band begins at `(2 − G)/r` of the source Nyquist with `G = 0.95`:
//! anything that could fold below 95 % of the output Nyquist (22.8 kHz at
//! 48 kHz) is in the stop band, and the passband reaches as high as the
//! transition width of the quality allows. Between unity and two semitones
//! up the cutoff eases in from the full band, where the only content that
//! can fold is the top 6 % of the source and it folds above 22.5 kHz.
//!
//! **Qualities.** [`Quality::Normal`] (8 zero crossings, 16 taps at unity,
//! about 37 at +12 st) for many voices, [`Quality::High`] (16 / 70) for
//! clips and effects, [`Quality::Offline`] (32 / 135) for renders off the
//! audio thread. Tables are built once ([`init`]); every read is
//! allocation-free and lock-free.

use crate::delay::DelayLine;
use std::sync::OnceLock;

/// Table samples per zero crossing of the kernel.
const PHASES: usize = 512;
/// Highest output frequency (fraction of Nyquist) aliases may fold to.
const GUARD: f64 = 0.95;
/// The cutoff reaches its full anti-alias value this many octaves up.
const RAMP_OCTAVES: f64 = 2.0 / 12.0;

/// How long a kernel the reads use (CPU against transition width).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Quality {
    /// 8 zero crossings, Kaiser β = 6.5 (≈ 68 dB stop band).
    Normal,
    /// 16 zero crossings, Kaiser β = 8.5 (≈ 86 dB).
    #[default]
    High,
    /// 32 zero crossings, Kaiser β = 10.5 (≈ 104 dB): offline renders.
    Offline,
}

impl Quality {
    /// `(zero crossings each side, Kaiser β, highest ratio the cutoff follows)`.
    const fn spec(self) -> (usize, f64, f64) {
        match self {
            Quality::Normal => (8, 6.5, 4.0),
            Quality::High => (16, 8.5, 4.0),
            Quality::Offline => (32, 10.5, 16.0),
        }
    }

    /// From a stepped parameter value (0 = Normal, 1 = High, 2 = Offline).
    pub fn from_index(i: usize) -> Self {
        match i {
            0 => Quality::Normal,
            2 => Quality::Offline,
            _ => Quality::High,
        }
    }
}

/// A tabulated windowed-sinc kernel for one [`Quality`].
#[derive(Debug)]
pub struct Kernel {
    zeros: usize,
    /// `h(i / PHASES)` for `i = 0 ..= zeros·PHASES` plus a zero guard.
    table: Box<[f32]>,
    /// `table[i + 1] − table[i]`, for the linear step between entries.
    delta: Box<[f32]>,
    /// Cutoff × ratio once fully anti-aliased: puts the stop band at `(2 − G)/r`.
    k: f64,
    /// Smallest cutoff (the cutoff stops following ratios above `1/min_fc`).
    min_fc: f32,
    /// Offline renders also keep images out when slowing down (cutoff `k`
    /// below unity), at the price of a slightly lower passband edge.
    guard_images: bool,
}

fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term, mut k) = (1.0, 1.0, 1.0);
    while term > 1e-14 * sum {
        term *= (x / (2.0 * k)) * (x / (2.0 * k));
        sum += term;
        k += 1.0;
    }
    sum
}

impl Kernel {
    /// Build the table (allocates; control thread).
    pub fn new(quality: Quality) -> Self {
        let (zeros, beta, max_ratio) = quality.spec();
        let n = zeros * PHASES;
        let i0 = bessel_i0(beta);
        let mut table = vec![0.0f32; n + 2];
        for (i, v) in table.iter_mut().enumerate().take(n + 1) {
            let x = i as f64 / PHASES as f64;
            // Exact zeros on the sinc's zeros keep whole-sample reads exact.
            let sinc = if i == 0 {
                1.0
            } else if i % PHASES == 0 {
                0.0
            } else {
                let px = std::f64::consts::PI * x;
                px.sin() / px
            };
            let r = x / zeros as f64;
            let win = if r < 1.0 { bessel_i0(beta * (1.0 - r * r).sqrt()) / i0 } else { 0.0 };
            *v = (sinc * win) as f32;
        }
        let delta: Vec<f32> = (0..n + 2).map(|i| if i + 1 < n + 2 { table[i + 1] - table[i] } else { 0.0 }).collect();
        // Kaiser's estimate of the transition width for `2·zeros` taps at
        // this β, as a fraction of the cutoff, half on each side of it.
        let atten = beta / 0.1102 + 8.7;
        let half_width = (atten - 8.0) / (2.285 * 2.0 * zeros as f64) / std::f64::consts::PI / 2.0;
        let k = (2.0 - GUARD) / (1.0 + half_width);
        Self { zeros, table: table.into(), delta: delta.into(), k, min_fc: (k / max_ratio) as f32, guard_images: quality == Quality::Offline }
    }

    /// The shared kernel of `quality`, built on first use. Call [`init`]
    /// off the audio thread first so that the audio thread only reads it.
    pub fn get(quality: Quality) -> &'static Kernel {
        static TABLES: [OnceLock<Kernel>; 3] = [OnceLock::new(), OnceLock::new(), OnceLock::new()];
        let slot = match quality {
            Quality::Normal => &TABLES[0],
            Quality::High => &TABLES[1],
            Quality::Offline => &TABLES[2],
        };
        slot.get_or_init(|| Kernel::new(quality))
    }

    /// Cutoff (fraction of the source Nyquist) for reading `ratio` source
    /// samples per output sample.
    #[inline]
    pub fn cutoff(&self, ratio: f64) -> f32 {
        let r = ratio.abs();
        if !r.is_finite() || r == 1.0 {
            return 1.0;
        }
        let ease = (r.log2().abs() / RAMP_OCTAVES).min(1.0);
        let k = 1.0 + (self.k - 1.0) * ease;
        if r > 1.0 {
            ((k / r) as f32).max(self.min_fc)
        } else if self.guard_images {
            k as f32
        } else {
            1.0
        }
    }

    /// Half the span of a read at cutoff `fc`, in source samples: a read at
    /// `pos` touches `pos − reach .. pos + reach`.
    #[inline]
    pub fn reach(&self, fc: f32) -> f32 {
        self.zeros as f32 / fc.clamp(self.min_fc, 1.0) + 1.0
    }

    /// Zero crossings each side of the kernel.
    pub fn zeros(&self) -> usize {
        self.zeros
    }

    /// Band-limited value at fractional position `pos`, with `at(i)` giving
    /// source sample `i` (any index; return 0 outside the data).
    #[inline]
    pub fn read_with(&self, pos: f64, fc: f32, at: impl Fn(i64) -> f32) -> f32 {
        let fc = if fc.is_finite() { fc.clamp(self.min_fc, 1.0) } else { 1.0 };
        if !pos.is_finite() {
            return 0.0;
        }
        let base = pos.floor();
        let i0 = base as i64;
        let frac = (pos - base) as f32;
        if frac == 0.0 && fc >= 1.0 {
            // Every other tap sits on a zero of the sinc.
            return at(i0);
        }
        let half = self.zeros as f32 / fc;
        let lo = (frac - half).floor() as i64 + 1;
        let hi = (frac + half).ceil() as i64 - 1;
        let scale = fc * PHASES as f32;
        let last = self.zeros * PHASES;
        let (mut acc, mut wsum) = (0.0f32, 0.0f32);
        for m in lo..=hi {
            let x = (m as f32 - frac).abs() * scale;
            let idx = x as usize;
            if idx >= last {
                continue;
            }
            let w = self.table[idx] + self.delta[idx] * (x - idx as f32);
            acc += w * at(i0 + m);
            wsum += w;
        }
        if wsum.abs() > 1e-6 { acc / wsum } else { 0.0 }
    }

    /// Band-limited read of `src` at `pos` (zero outside the slice).
    #[inline]
    pub fn read(&self, src: &[f32], pos: f64, fc: f32) -> f32 {
        let len = src.len() as i64;
        self.read_with(pos, fc, |i| if i >= 0 && i < len { src[i as usize] } else { 0.0 })
    }

    /// Band-limited read of a delay line `delay` samples back. The delay is
    /// kept where the whole kernel fits behind the write head and inside the
    /// line, so a head that reads close to the input keeps a small offset.
    #[inline]
    pub fn read_delay(&self, line: &DelayLine, delay: f32, fc: f32) -> f32 {
        let reach = self.reach(fc);
        let max = line.max_delay() as f32 - reach;
        let d = delay.clamp(reach, max.max(reach));
        self.read_with(d as f64, fc, |i| line.read(i.max(0) as usize))
    }
}

/// Build every kernel table now (call off the audio thread, e.g. in a
/// processor's constructor or `prepare`).
pub fn init() {
    for q in [Quality::Normal, Quality::High, Quality::Offline] {
        let _ = Kernel::get(q);
    }
}

/// Offline: resample planar `channels` to `out_len` frames (the source read
/// at `in_len / out_len` frames per output frame from frame 0), band-limited
/// at `quality`. Equal lengths copy the input.
pub fn resample(channels: &[&[f32]], out_len: usize, quality: Quality) -> Vec<Vec<f32>> {
    let in_len = channels.first().map_or(0, |c| c.len());
    if in_len == 0 || out_len == 0 {
        return channels.iter().map(|_| Vec::new()).collect();
    }
    if in_len == out_len {
        return channels.iter().map(|c| c.to_vec()).collect();
    }
    let step = in_len as f64 / out_len as f64;
    let kernel = Kernel::get(quality);
    let fc = kernel.cutoff(step);
    channels.iter().map(|ch| (0..out_len).map(|i| kernel.read(ch, i as f64 * step, fc)).collect()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::TAU;

    const SR: f64 = 48_000.0;

    fn sine(freq: f64, n: usize) -> Vec<f32> {
        (0..n).map(|i| (0.5 * (TAU * freq * i as f64 / SR).sin()) as f32).collect()
    }

    fn rms(x: &[f32]) -> f64 {
        (x.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / x.len().max(1) as f64).sqrt()
    }

    /// Read `x` at `ratio` frames per output frame, the steady middle part.
    fn play(q: Quality, x: &[f32], ratio: f64) -> Vec<f32> {
        let k = Kernel::get(q);
        let fc = k.cutoff(ratio);
        let n = ((x.len() as f64 - 400.0) / ratio) as usize;
        (200..n).map(|i| k.read(x, 37.25 + i as f64 * ratio, fc)).collect()
    }

    fn gain_db(q: Quality, freq: f64, ratio: f64) -> f64 {
        let x = sine(freq, 48_000);
        20.0 * (rms(&play(q, &x, ratio)) / (0.5 / 2f64.sqrt())).log10()
    }

    #[test]
    fn whole_samples_at_unity_are_bit_exact() {
        let x = sine(1234.5, 2_000);
        for q in [Quality::Normal, Quality::High, Quality::Offline] {
            let k = Kernel::get(q);
            assert_eq!(k.cutoff(1.0), 1.0);
            for i in [0usize, 1, 7, 500, 1_999] {
                assert_eq!(k.read(&x, i as f64, 1.0).to_bits(), x[i].to_bits(), "{q:?} frame {i}");
            }
        }
    }

    #[test]
    fn dc_is_exact_at_any_cutoff() {
        let x = vec![0.25f32; 4_000];
        for q in [Quality::Normal, Quality::High, Quality::Offline] {
            let k = Kernel::get(q);
            for ratio in [0.5, 1.0, 1.3, 2.0, 3.7] {
                let v = k.read(&x, 2_000.37, k.cutoff(ratio));
                assert!((v - 0.25).abs() < 1e-6, "{q:?} ratio {ratio}: {v}");
            }
        }
    }

    /// Report 24.01 §11 (`warpalias`): a 13 kHz sine read twice as fast must
    /// vanish (26 kHz is past Nyquist); Hermite left it at full level at 22 kHz.
    #[test]
    fn an_octave_up_does_not_fold_13_khz_back() {
        let x = sine(13_000.0, 96_000);
        for (q, limit) in [(Quality::Normal, -60.0), (Quality::High, -80.0), (Quality::Offline, -95.0)] {
            let db = 20.0 * (rms(&play(q, &x, 2.0)) / rms(&x)).log10();
            eprintln!("{q:?}: 13 kHz at ratio 2 leaves {db:.1} dB");
            assert!(db < limit, "{q:?}: {db:.1} dB");
        }
        // Further up the cutoff keeps following: +24 st on 7 kHz (28 kHz).
        let x = sine(7_000.0, 96_000);
        for q in [Quality::Normal, Quality::High, Quality::Offline] {
            let db = 20.0 * (rms(&play(q, &x, 4.0)) / rms(&x)).log10();
            assert!(db < -55.0, "{q:?} +24 st: {db:.1} dB");
        }
    }

    #[test]
    fn passband_is_flat() {
        // An octave up: what lands below 15 / 18 / 20 kHz keeps its level.
        for (q, top, tol) in [(Quality::Normal, 7_000.0, 0.1), (Quality::High, 8_500.0, 0.05), (Quality::Offline, 9_500.0, 0.02)] {
            for f in [100.0, 1_000.0, 4_000.0, top] {
                let g = gain_db(q, f, 2.0);
                assert!(g.abs() < tol, "{q:?} ratio 2, {f} Hz: {g:+.3} dB");
            }
        }
        // Fractional reads near unity (a 44.1 kHz file at 48 kHz, a small
        // detune, a fifth down): flat to 15 kHz (Normal) or 18 kHz.
        for q in [Quality::Normal, Quality::High, Quality::Offline] {
            let top = if q == Quality::Normal { 15_000.0 } else { 18_000.0 };
            for ratio in [0.91875f64, 1.0, 1.01, 0.667] {
                for f in [100.0, 5_000.0, 12_000.0, top * ratio.min(1.0)] {
                    let g = gain_db(q, f, ratio);
                    let tol = if q == Quality::Normal { 0.15 } else { 0.05 };
                    assert!(g.abs() < tol, "{q:?} ratio {ratio}, {f} Hz: {g:+.3} dB");
                }
            }
        }
    }

    #[test]
    fn offline_resample_matches_lengths_and_copies_at_unity() {
        let x = sine(440.0, 10_000);
        let same = resample(&[&x], 10_000, Quality::Offline);
        assert_eq!(same[0], x);
        let half = resample(&[&x, &x], 5_000, Quality::Offline);
        assert_eq!(half.len(), 2);
        assert_eq!(half[0].len(), 5_000);
        assert!(resample(&[&[]], 10, Quality::Offline)[0].is_empty());
    }

    #[test]
    fn delay_reads_stay_inside_the_line() {
        let k = Kernel::get(Quality::High);
        let mut line = DelayLine::new(512);
        for i in 0..600 {
            line.push((i as f32 * 0.01).sin());
        }
        let fc = k.cutoff(2.0);
        for d in [0.0f32, 1.0, 100.5, 511.0, 10_000.0] {
            assert!(k.read_delay(&line, d, fc).is_finite());
        }
        // At unity a whole delay is the stored sample.
        assert_eq!(k.read_delay(&line, 100.0, 1.0), line.read(100));
    }
}
