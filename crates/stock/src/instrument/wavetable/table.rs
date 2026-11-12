//! Band-limited, mip-mapped wavetables and the lock-free mailbox that hands
//! prepared tables (and other heavy objects) to the audio thread.
//!
//! A [`MipTable`] holds every frame of a wavetable at [`LEVELS`] octave
//! levels. Level `l` keeps the first `MAX_HARMONICS >> l` harmonics of the
//! frame (measured with a forward FFT of the 2048-sample cycle and rebuilt
//! with an inverse FFT) and is stored at four times that many samples, so
//! the 4-point Hermite read between samples stays clean. The oscillator picks
//! the fullest level whose top harmonic stays below Nyquist, so no partial
//! ever folds back. Building happens off the audio thread; reading never
//! allocates.

use realfft::RealFftPlanner;
use realfft::num_complex::Complex;
use std::sync::atomic::{AtomicPtr, Ordering};

/// Samples per frame of the source cycles (Serum / Vital convention).
pub const FRAME_LEN: usize = 2048;
/// Most frames a table may hold (Serum's limit).
pub const MAX_FRAMES: usize = 256;
/// Harmonics kept in the fullest level (a 47 Hz note reaches 24 kHz at 48 kHz).
pub const MAX_HARMONICS: usize = 512;
/// Octave levels: 512, 256, … 1 harmonics.
pub const LEVELS: usize = 10;
/// Table samples per cycle of the top harmonic.
const OVERSAMPLE: usize = 4;
/// Smallest stored level (the thin levels are heavily oversampled).
const MIN_SIZE: usize = 256;
/// Guard samples around each level: one before, two after (Hermite taps).
const GUARD: usize = 3;

#[derive(Clone, Copy, Debug, Default)]
struct Level {
    /// Index of sample 0 inside a frame's block.
    offset: usize,
    size: usize,
    harmonics: usize,
}

/// How [`MipTable::build`] scales the frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Normalize {
    /// Keep the source levels.
    None,
    /// One gain for the whole table: its loudest frame peaks at 1.
    Table,
    /// Every frame peaks at 1 (the classic factory tables).
    PerFrame,
}

/// A morphing, mip-mapped, band-limited wavetable.
#[derive(Clone, Debug)]
pub struct MipTable {
    frames: usize,
    levels: [Level; LEVELS],
    stride: usize,
    data: Vec<f32>,
}

fn layout() -> ([Level; LEVELS], usize) {
    let mut levels = [Level::default(); LEVELS];
    let mut offset = 1;
    for (l, lv) in levels.iter_mut().enumerate() {
        let harmonics = (MAX_HARMONICS >> l).max(1);
        let size = (harmonics * OVERSAMPLE).max(MIN_SIZE);
        *lv = Level { offset, size, harmonics };
        offset += size + GUARD;
    }
    (levels, offset - 1)
}

impl MipTable {
    /// Build from single cycles of any length (each is analysed with its own
    /// FFT, so a 600-sample cycle works as well as a 2048-sample frame). At
    /// most [`MAX_FRAMES`] frames are used; an empty list gives a sine.
    pub fn build(cycles: &[Vec<f32>], normalize: Normalize) -> Self {
        let mut planner = RealFftPlanner::<f32>::new();
        let spectra: Vec<Vec<Complex<f32>>> = cycles
            .iter()
            .take(MAX_FRAMES)
            .map(|c| {
                let n = c.len().max(16);
                let fft = planner.plan_fft_forward(n);
                let mut input = c.clone();
                input.resize(n, 0.0);
                for v in &mut input {
                    if !v.is_finite() {
                        *v = 0.0;
                    }
                }
                let mut out = fft.make_output_vec();
                let _ = fft.process(&mut input, &mut out);
                let scale = 1.0 / n as f32;
                (0..=MAX_HARMONICS).map(|k| if k == 0 { Complex::new(0.0, 0.0) } else { out.get(k).copied().unwrap_or_default() * scale }).collect()
            })
            .collect();
        if spectra.is_empty() {
            return Self::from_spectra(&[sine_spectrum()], normalize, &mut planner);
        }
        Self::from_spectra(&spectra, normalize, &mut planner)
    }

    /// Build from harmonic amplitudes: `spectrum(frame, k)` gives the
    /// `(sin, cos)` amplitudes of harmonic `k >= 1`.
    pub fn from_harmonics(frames: usize, normalize: Normalize, spectrum: impl Fn(usize, usize) -> (f32, f32)) -> Self {
        let spectra: Vec<Vec<Complex<f32>>> = (0..frames.clamp(1, MAX_FRAMES))
            .map(|f| (0..=MAX_HARMONICS).map(|k| if k == 0 { Complex::new(0.0, 0.0) } else { harmonic_bin(spectrum(f, k)) }).collect())
            .collect();
        Self::from_spectra(&spectra, normalize, &mut RealFftPlanner::new())
    }

    /// `spectra[frame][k]` are half-spectrum bins scaled so that
    /// `x[n] = Σ 2 Re(X_k e^{j2πkn/N})`.
    fn from_spectra(spectra: &[Vec<Complex<f32>>], normalize: Normalize, planner: &mut RealFftPlanner<f32>) -> Self {
        let (levels, stride) = layout();
        let frames = spectra.len().max(1);
        let mut data = vec![0.0f32; stride * frames];
        let mut peaks = vec![0.0f32; frames];
        for (f, spec) in spectra.iter().enumerate() {
            for (l, lv) in levels.iter().enumerate() {
                let ifft = planner.plan_fft_inverse(lv.size);
                let mut bins = ifft.make_input_vec();
                let top = lv.harmonics.min(lv.size / 2 - 1);
                for (k, bin) in bins.iter_mut().enumerate().take(top + 1).skip(1) {
                    *bin = spec.get(k).copied().unwrap_or_default();
                }
                let mut out = ifft.make_output_vec();
                let _ = ifft.process(&mut bins, &mut out);
                let base = f * stride + lv.offset;
                data[base..base + lv.size].copy_from_slice(&out);
                data[base - 1] = out[lv.size - 1];
                data[base + lv.size] = out[0];
                data[base + lv.size + 1] = out[1];
                if l == 0 {
                    peaks[f] = out.iter().fold(0.0f32, |m, v| m.max(v.abs()));
                }
            }
        }
        let gain = |f: usize| match normalize {
            Normalize::None => 1.0,
            Normalize::Table => {
                let m = peaks.iter().fold(0.0f32, |a, &b| a.max(b));
                if m > 1e-9 { 1.0 / m } else { 1.0 }
            }
            Normalize::PerFrame => {
                if peaks[f] > 1e-9 {
                    1.0 / peaks[f]
                } else {
                    1.0
                }
            }
        };
        for f in 0..frames {
            let g = gain(f);
            if g != 1.0 {
                for v in &mut data[f * stride..(f + 1) * stride] {
                    *v *= g;
                }
            }
        }
        Self { frames, levels, stride, data }
    }

    pub fn frames(&self) -> usize {
        self.frames
    }

    /// Bytes of sample data (for memory budgets in the UI).
    pub fn bytes(&self) -> usize {
        self.data.len() * 4
    }

    /// Fullest level for a phase increment `ratio = f0 / fs` (multiply by the
    /// warp's speed-up first).
    #[inline]
    pub fn level_for(&self, ratio: f32) -> usize {
        let max_h = 0.5 / ratio.max(1e-9);
        let mut l = 0;
        while l < LEVELS - 1 && self.levels[l].harmonics as f32 > max_h {
            l += 1;
        }
        l
    }

    /// Harmonics kept by `level`.
    pub fn level_harmonics(&self, level: usize) -> usize {
        self.levels[level.min(LEVELS - 1)].harmonics
    }

    #[inline]
    fn read(&self, frame: usize, level: usize, phase: f32) -> f32 {
        let lv = &self.levels[level];
        let pos = phase * lv.size as f32;
        let i = (pos as usize).min(lv.size - 1);
        let t = pos - i as f32;
        let b = frame * self.stride + lv.offset + i;
        // Guards make b - 1 ..= b + 2 always valid.
        let (xm1, x0, x1, x2) = (self.data[b - 1], self.data[b], self.data[b + 1], self.data[b + 2]);
        hermite(xm1, x0, x1, x2, t)
    }

    /// Sample at `phase ∈ [0, 1)` and frame morph `position ∈ [0, 1]`.
    #[inline]
    pub fn sample(&self, level: usize, position: f32, phase: f32) -> f32 {
        let level = level.min(LEVELS - 1);
        let phase = if (0.0..1.0).contains(&phase) { phase } else { phase.rem_euclid(1.0) % 1.0 };
        if self.frames == 1 {
            return self.read(0, level, phase);
        }
        let fp = position.clamp(0.0, 1.0) * (self.frames - 1) as f32;
        let f0 = (fp as usize).min(self.frames - 2);
        let t = fp - f0 as f32;
        let a = self.read(f0, level, phase);
        if t <= 0.0 {
            return a;
        }
        let b = self.read(f0 + 1, level, phase);
        a + (b - a) * t
    }

    /// One frame (morph `position`) of `level` drawn at `n` points, for UI
    /// previews.
    pub fn preview(&self, position: f32, n: usize) -> Vec<f32> {
        (0..n).map(|i| self.sample(0, position, i as f32 / n as f32)).collect()
    }
}

fn sine_spectrum() -> Vec<Complex<f32>> {
    (0..=MAX_HARMONICS).map(|k| if k == 1 { harmonic_bin((1.0, 0.0)) } else { Complex::new(0.0, 0.0) }).collect()
}

/// `a sin + b cos` → half-spectrum bin.
fn harmonic_bin((s, c): (f32, f32)) -> Complex<f32> {
    Complex::new(c * 0.5, -s * 0.5)
}

/// 4-point, 3rd-order Hermite interpolation between `x0` and `x1`.
#[inline]
pub fn hermite(xm1: f32, x0: f32, x1: f32, x2: f32, t: f32) -> f32 {
    let c = (x1 - xm1) * 0.5;
    let v = x0 - x1;
    let w = c + v;
    let a = w + v + (x2 - x0) * 0.5;
    let b = w + a;
    ((a * t - b) * t + c) * t + x0
}

// ── Lock-free hand-over ─────────────────────────────────────────────────────

/// A one-slot mailbox from a control thread to the audio thread, with a
/// return slot so the audio thread never frees memory.
///
/// * Control thread: [`post`](Mailbox::post) a boxed value (an unconsumed
///   earlier post is dropped there), and call [`collect`](Mailbox::collect)
///   now and then to free what the audio thread retired.
/// * Audio thread: [`swap_into`](Mailbox::swap_into) exchanges the posted
///   value with its current one and parks the old value in the return slot.
///   It only takes a new value once the return slot is empty, so it never
///   has to drop anything.
pub struct Mailbox<T> {
    inbox: AtomicPtr<T>,
    outbox: AtomicPtr<T>,
}

impl<T> Default for Mailbox<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Mailbox<T> {
    pub const fn new() -> Self {
        Self { inbox: AtomicPtr::new(std::ptr::null_mut()), outbox: AtomicPtr::new(std::ptr::null_mut()) }
    }

    /// Control thread: offer a new value.
    pub fn post(&self, value: Box<T>) {
        let old = self.inbox.swap(Box::into_raw(value), Ordering::AcqRel);
        if !old.is_null() {
            // SAFETY: a non-null inbox pointer came from `Box::into_raw` and
            // the swap made us its only owner.
            drop(unsafe { Box::from_raw(old) });
        }
        self.collect();
    }

    /// Control thread: free what the audio thread retired. Returns it.
    pub fn collect(&self) -> Option<Box<T>> {
        let old = self.outbox.swap(std::ptr::null_mut(), Ordering::AcqRel);
        // SAFETY: as in `post`; the audio thread gave up ownership.
        if old.is_null() { None } else { Some(unsafe { Box::from_raw(old) }) }
    }

    /// True when a posted value waits for the audio thread.
    pub fn pending(&self) -> bool {
        !self.inbox.load(Ordering::Acquire).is_null()
    }

    /// Audio thread: take the posted value into `current`, parking the old
    /// one for [`collect`](Mailbox::collect). Allocation and lock free.
    /// Returns true when `current` changed.
    pub fn swap_into(&self, current: &mut Box<T>) -> bool {
        if !self.outbox.load(Ordering::Acquire).is_null() || self.inbox.load(Ordering::Acquire).is_null() {
            return false;
        }
        let new = self.inbox.swap(std::ptr::null_mut(), Ordering::AcqRel);
        if new.is_null() {
            return false;
        }
        // SAFETY: `new` came from `Box::into_raw` and the swap made us its owner.
        let old = std::mem::replace(current, unsafe { Box::from_raw(new) });
        self.outbox.store(Box::into_raw(old), Ordering::Release);
        true
    }
}

impl<T> Drop for Mailbox<T> {
    fn drop(&mut self) {
        for p in [self.inbox.swap(std::ptr::null_mut(), Ordering::AcqRel), self.outbox.swap(std::ptr::null_mut(), Ordering::AcqRel)] {
            if !p.is_null() {
                // SAFETY: both slots only ever hold `Box::into_raw` pointers.
                drop(unsafe { Box::from_raw(p) });
            }
        }
    }
}

// SAFETY: the mailbox moves `T` between threads; the atomics give each
// pointer a single owner at a time.
unsafe impl<T: Send> Send for Mailbox<T> {}
unsafe impl<T: Send> Sync for Mailbox<T> {}

#[cfg(test)]
mod tests {
    use super::*;

    fn saw_cycle(n: usize) -> Vec<f32> {
        (0..n).map(|i| 2.0 * i as f32 / n as f32 - 1.0).collect()
    }

    /// Worst non-harmonic peak relative to the fundamental, in dB, of a tone
    /// at `hz` read at `position`.
    pub(crate) fn alias_db(table: &MipTable, hz: f32, position: f32) -> f32 {
        let sr = 48_000.0f32;
        let n = 1 << 15;
        let inc = hz / sr;
        let level = table.level_for(inc);
        let mut ph = 0.0f32;
        let mut x: Vec<f32> = (0..n)
            .map(|i| {
                let w = 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos();
                let y = table.sample(level, position, ph) * w;
                ph += inc;
                if ph >= 1.0 {
                    ph -= 1.0;
                }
                y
            })
            .collect();
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(n);
        let mut out = fft.make_output_vec();
        fft.process(&mut x, &mut out).unwrap();
        let spec: Vec<f32> = out.iter().map(|c| c.norm()).collect();
        let bin_hz = sr / n as f32;
        let fundamental = spec[(hz / bin_hz).round() as usize];
        let mut worst = 0.0f32;
        for (i, &m) in spec.iter().enumerate() {
            let f = i as f32 * bin_hz;
            let nearest = (f / hz).round() * hz;
            if (f - nearest).abs() > 40.0 && f > 30.0 {
                worst = worst.max(m);
            }
        }
        20.0 * (worst / fundamental).log10()
    }

    #[test]
    fn saw_is_alias_free_up_high() {
        let t = MipTable::build(&[saw_cycle(FRAME_LEN)], Normalize::PerFrame);
        for hz in [440.0, 1975.5, 3100.0, 5274.0, 8372.0] {
            let db = alias_db(&t, hz, 0.0);
            assert!(db < -70.0, "{hz} Hz: alias {db} dB");
        }
    }

    #[test]
    fn naive_reading_does_aliase() {
        // The measurement is meaningful: the raw 2048-sample saw, read with
        // no mip-mapping, aliases badly.
        let raw = saw_cycle(FRAME_LEN);
        let sr = 48_000.0f32;
        let hz = 3100.0;
        let n = 1 << 15;
        let mut ph = 0.0f32;
        let mut x: Vec<f32> = (0..n)
            .map(|i| {
                let w = 0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos();
                let y = raw[(ph * FRAME_LEN as f32) as usize % FRAME_LEN] * w;
                ph = (ph + hz / sr).fract();
                y
            })
            .collect();
        let mut planner = RealFftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(n);
        let mut out = fft.make_output_vec();
        fft.process(&mut x, &mut out).unwrap();
        let bin = sr / n as f32;
        let fund = out[(hz / bin).round() as usize].norm();
        let worst = out
            .iter()
            .enumerate()
            .filter(|(i, _)| {
                let f = *i as f32 * bin;
                (f - (f / hz).round() * hz).abs() > 40.0 && f > 30.0
            })
            .map(|(_, c)| c.norm())
            .fold(0.0f32, f32::max);
        assert!(20.0 * (worst / fund).log10() > -50.0);
    }

    #[test]
    fn morph_interpolates_frames() {
        let sine: Vec<f32> = (0..FRAME_LEN).map(|i| (std::f32::consts::TAU * i as f32 / FRAME_LEN as f32).sin()).collect();
        let neg: Vec<f32> = sine.iter().map(|v| -v).collect();
        let t = MipTable::build(&[sine, neg], Normalize::None);
        assert_eq!(t.frames(), 2);
        assert!((t.sample(0, 0.0, 0.25) - 1.0).abs() < 1e-3);
        assert!((t.sample(0, 1.0, 0.25) + 1.0).abs() < 1e-3);
        assert!(t.sample(0, 0.5, 0.25).abs() < 1e-3);
    }

    #[test]
    fn normalisation_modes() {
        let half: Vec<f32> = saw_cycle(FRAME_LEN).iter().map(|v| v * 0.25).collect();
        let full = saw_cycle(FRAME_LEN);
        let peak = |t: &MipTable, pos: f32| (0..997).map(|i| t.sample(0, pos, i as f32 / 997.0).abs()).fold(0.0f32, f32::max);
        let t = MipTable::build(&[half.clone(), full.clone()], Normalize::PerFrame);
        assert!((peak(&t, 0.0) - 1.0).abs() < 0.05 && (peak(&t, 1.0) - 1.0).abs() < 0.05);
        let t = MipTable::build(&[half.clone(), full.clone()], Normalize::Table);
        assert!((peak(&t, 0.0) - 0.25).abs() < 0.03, "{}", peak(&t, 0.0));
        let t = MipTable::build(&[half], Normalize::None);
        assert!((peak(&t, 0.0) - 0.25 * 1.09).abs() < 0.05, "{}", peak(&t, 0.0));
    }

    #[test]
    fn short_cycles_and_levels() {
        // Same harmonics whatever the cycle length (compared unnormalised:
        // the Gibbs peak, and so a peak normalisation, depends on it).
        let t = MipTable::build(&[saw_cycle(600)], Normalize::None);
        let s = MipTable::build(&[saw_cycle(FRAME_LEN)], Normalize::None);
        for i in 1..50 {
            let ph = i as f32 / 50.0 + 0.003;
            assert!((t.sample(8, 0.0, ph) - s.sample(8, 0.0, ph)).abs() < 0.05, "{ph} {} {}", t.sample(8, 0.0, ph), s.sample(8, 0.0, ph));
        }
        assert_eq!(s.level_for(20.0 / 48_000.0), 0);
        let l = s.level_for(1000.0 / 48_000.0);
        assert!(s.level_harmonics(l) as f32 * 1000.0 <= 24_000.0);
        assert!(s.level_harmonics(l - 1) as f32 * 1000.0 > 24_000.0);
        // Memory: ~21 kB a frame.
        assert!(s.bytes() < 24_000, "{}", s.bytes());
    }

    #[test]
    fn mailbox_hands_over_without_rt_drops() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicUsize;
        struct Counted(Arc<AtomicUsize>);
        impl Drop for Counted {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::SeqCst);
            }
        }
        let drops = Arc::new(AtomicUsize::new(0));
        let mb = Mailbox::<Counted>::new();
        let mut current = Box::new(Counted(drops.clone()));
        assert!(!mb.swap_into(&mut current));
        mb.post(Box::new(Counted(drops.clone())));
        mb.post(Box::new(Counted(drops.clone()))); // replaces the first, dropped here
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert!(mb.swap_into(&mut current));
        // The old value waits in the outbox, not dropped on the "RT" side.
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        mb.post(Box::new(Counted(drops.clone())));
        // Outbox was full: post() collected it first, so the swap may proceed.
        assert_eq!(drops.load(Ordering::SeqCst), 2);
        assert!(mb.swap_into(&mut current));
        // Outbox full again: a further post waits until collected.
        let pending = Box::new(Counted(drops.clone()));
        mb.inbox.store(Box::into_raw(pending), Ordering::Release);
        assert!(!mb.swap_into(&mut current));
        assert!(mb.collect().is_some());
        assert!(mb.swap_into(&mut current));
        drop(mb);
        drop(current);
        assert_eq!(drops.load(Ordering::SeqCst), 5);
    }
}
