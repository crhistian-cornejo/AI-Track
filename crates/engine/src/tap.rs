//! Lock-free taps the audio thread feeds for the UI's analysers.
//!
//! * [`AnalysisTap`]: a ring of stereo frames written after the mix (or after
//!   one track's fader). The writer never waits: it overwrites the oldest
//!   frames, and a [`TapReader`] that fell more than the capacity behind
//!   skips ahead and reports how many frames it lost. The UI drains it once
//!   per frame and runs the FFT / loudness maths on its own thread.
//! * [`DeviceMeter`]: the values of one processor's `meter()` (spectrum,
//!   gain reduction…), published once per block while a reader is interested.
//!
//! Both are allocated off the audio thread; the audio thread only does
//! atomic loads and stores.

use aitrack_model::TrackId;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

/// Frames the analysis ring holds (about 1.4 s at 48 kHz).
pub const TAP_FRAMES: usize = 1 << 16;
/// Frames next to the write position a reader never trusts: the writer
/// stores a whole block before it publishes the new position.
const TAP_GUARD: u64 = 8192;
/// Most values a device meter carries.
pub const DEVICE_METER_LEN: usize = 320;
/// Blocks a device keeps publishing after the last read.
const METER_INTEREST: u32 = 96;

const SOURCE_MAIN: u64 = 0;

/// Stereo analysis ring: one writer (the audio thread), any number of readers.
pub struct AnalysisTap {
    enabled: AtomicBool,
    /// `0` = the Main output, otherwise `track id + 1`.
    source: AtomicU64,
    /// Frames written since creation.
    written: AtomicU64,
    /// Interleaved `L, R` as `f32` bits.
    buf: Box<[AtomicU32]>,
    mask: u64,
}

impl AnalysisTap {
    pub fn new(frames: usize) -> Self {
        let frames = frames.max(2 * TAP_GUARD as usize).next_power_of_two();
        Self {
            enabled: AtomicBool::new(false),
            source: AtomicU64::new(SOURCE_MAIN),
            written: AtomicU64::new(0),
            buf: (0..frames * 2).map(|_| AtomicU32::new(0)).collect(),
            mask: frames as u64 - 1,
        }
    }

    pub fn capacity(&self) -> usize {
        self.mask as usize + 1
    }

    #[inline]
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Audio thread: whether the Main output is what readers want.
    #[inline]
    pub(crate) fn wants_main(&self) -> bool {
        self.is_enabled() && self.source.load(Ordering::Relaxed) == SOURCE_MAIN
    }

    /// Audio thread: whether `track`'s output is what readers want.
    #[inline]
    pub(crate) fn wants_track(&self, track: TrackId) -> bool {
        self.is_enabled() && self.source.load(Ordering::Relaxed) == track.0 + 1
    }

    /// Audio thread: append a block. Wait-free; overwrites the oldest frames.
    #[inline]
    pub(crate) fn write(&self, left: &[f32], right: &[f32]) {
        let mut w = self.written.load(Ordering::Relaxed);
        for (l, r) in left.iter().zip(right) {
            let i = ((w & self.mask) * 2) as usize;
            self.buf[i].store(l.to_bits(), Ordering::Relaxed);
            self.buf[i + 1].store(r.to_bits(), Ordering::Relaxed);
            w += 1;
        }
        self.written.store(w, Ordering::Release);
    }
}

/// A reader's cursor into an [`AnalysisTap`].
pub struct TapReader {
    tap: Arc<AnalysisTap>,
    read: u64,
}

impl TapReader {
    pub fn new(tap: Arc<AnalysisTap>) -> Self {
        let read = tap.written.load(Ordering::Acquire);
        Self { tap, read }
    }

    /// Turn the tap on or off. Off, the audio thread skips it entirely.
    pub fn set_enabled(&mut self, on: bool) {
        if on && !self.tap.is_enabled() {
            self.skip_to_now();
        }
        self.tap.enabled.store(on, Ordering::Relaxed);
    }

    pub fn is_enabled(&self) -> bool {
        self.tap.is_enabled()
    }

    /// Tap the Main output (`None`) or one track's post-fader output.
    pub fn set_source(&mut self, track: Option<TrackId>) {
        let v = track.map_or(SOURCE_MAIN, |t| t.0 + 1);
        if self.tap.source.swap(v, Ordering::Relaxed) != v {
            self.skip_to_now();
        }
    }

    /// Frames a reader can fall behind before it loses the oldest ones.
    pub fn capacity(&self) -> usize {
        self.tap.capacity() - TAP_GUARD as usize
    }

    /// Frames waiting to be read.
    pub fn available(&self) -> usize {
        (self.tap.written.load(Ordering::Acquire) - self.read).min(self.capacity() as u64) as usize
    }

    /// Forget everything written so far.
    pub fn skip_to_now(&mut self) {
        self.read = self.tap.written.load(Ordering::Acquire);
    }

    /// Append every new frame to `out`, oldest first. Returns how many
    /// frames were lost because the reader fell too far behind.
    pub fn read(&mut self, out: &mut Vec<[f32; 2]>) -> u64 {
        let cap = self.capacity() as u64;
        let w = self.tap.written.load(Ordering::Acquire);
        let mut start = self.read.min(w);
        let mut dropped = 0;
        if w - start > cap {
            dropped = w - start - cap;
            start = w - cap;
        }
        let base = out.len();
        out.reserve((w - start) as usize);
        for i in start..w {
            let k = ((i & self.tap.mask) * 2) as usize;
            out.push([f32::from_bits(self.tap.buf[k].load(Ordering::Relaxed)), f32::from_bits(self.tap.buf[k + 1].load(Ordering::Relaxed))]);
        }
        // The writer may have lapped the oldest frames during the copy.
        let w2 = self.tap.written.load(Ordering::Acquire);
        if w2 > start + cap {
            let lost = (w2 - cap - start).min(w - start);
            out.drain(base..base + lost as usize);
            dropped += lost;
        }
        self.read = w;
        dropped
    }
}

/// The latest `Processor::meter()` values of one device.
pub struct DeviceMeter {
    interest: AtomicU32,
    len: AtomicU32,
    data: Box<[AtomicU32]>,
}

impl Default for DeviceMeter {
    fn default() -> Self {
        Self { interest: AtomicU32::new(0), len: AtomicU32::new(0), data: (0..DEVICE_METER_LEN).map(|_| AtomicU32::new(0)).collect() }
    }
}

impl DeviceMeter {
    /// Audio thread, once per block: whether a reader asked recently.
    #[inline]
    pub(crate) fn wanted(&self) -> bool {
        let n = self.interest.load(Ordering::Relaxed);
        if n == 0 {
            return false;
        }
        self.interest.store(n - 1, Ordering::Relaxed);
        true
    }

    /// Audio thread: publish the processor's values.
    #[inline]
    pub(crate) fn publish(&self, values: &[f32]) {
        let n = values.len().min(self.data.len());
        for (slot, v) in self.data.iter().zip(&values[..n]) {
            slot.store(v.to_bits(), Ordering::Relaxed);
        }
        self.len.store(n as u32, Ordering::Release);
    }

    /// UI thread: copy the latest values and keep the device publishing for
    /// a while. Returns how many values were written to `out`.
    pub fn read(&self, out: &mut [f32]) -> usize {
        self.interest.store(METER_INTEREST, Ordering::Relaxed);
        let n = (self.len.load(Ordering::Acquire) as usize).min(out.len());
        for (o, slot) in out.iter_mut().zip(self.data.iter()).take(n) {
            *o = f32::from_bits(slot.load(Ordering::Relaxed));
        }
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(from: u64, n: usize) -> (Vec<f32>, Vec<f32>) {
        ((0..n).map(|i| (from + i as u64) as f32).collect(), (0..n).map(|i| -((from + i as u64) as f32)).collect())
    }

    #[test]
    fn frames_arrive_in_order() {
        let tap = Arc::new(AnalysisTap::new(TAP_FRAMES));
        let mut r = TapReader::new(tap.clone());
        r.set_enabled(true);
        let mut out = Vec::new();
        let mut pos = 0;
        for n in [64usize, 256, 1, 1024, 333] {
            let (l, rr) = block(pos, n);
            tap.write(&l, &rr);
            pos += n as u64;
        }
        assert_eq!(r.available(), pos as usize);
        assert_eq!(r.read(&mut out), 0);
        assert_eq!(out.len() as u64, pos);
        for (i, f) in out.iter().enumerate() {
            assert_eq!(*f, [i as f32, -(i as f32)]);
        }
        assert_eq!(r.read(&mut out), 0);
        assert_eq!(out.len() as u64, pos, "nothing new");
    }

    #[test]
    fn an_overrun_keeps_the_newest_frames_and_reports_the_loss() {
        let tap = Arc::new(AnalysisTap::new(TAP_FRAMES));
        let mut r = TapReader::new(tap.clone());
        r.set_enabled(true);
        let cap = r.capacity() as u64;
        // Write three times the capacity without reading.
        let total = 3 * tap.capacity() as u64;
        let mut pos = 0;
        while pos < total {
            let (l, rr) = block(pos, 4096);
            tap.write(&l, &rr);
            pos += 4096;
        }
        let mut out = Vec::new();
        let dropped = r.read(&mut out);
        assert_eq!(dropped, total - cap);
        assert_eq!(out.len() as u64, cap);
        assert_eq!(out[0][0], (total - cap) as f32, "oldest surviving frame");
        assert_eq!(out.last().unwrap()[0], (total - 1) as f32);
        // Back in step afterwards.
        let (l, rr) = block(pos, 128);
        tap.write(&l, &rr);
        out.clear();
        assert_eq!(r.read(&mut out), 0);
        assert_eq!(out.len(), 128);
        assert_eq!(out[0][0], pos as f32);
    }

    #[test]
    fn enabling_and_switching_source_start_from_now() {
        let tap = Arc::new(AnalysisTap::new(TAP_FRAMES));
        let mut r = TapReader::new(tap.clone());
        assert!(!tap.wants_main());
        let (l, rr) = block(0, 512);
        tap.write(&l, &rr);
        r.set_enabled(true);
        assert!(tap.wants_main() && !tap.wants_track(TrackId(3)));
        let mut out = Vec::new();
        r.read(&mut out);
        assert!(out.is_empty(), "frames from before enabling are skipped");
        r.set_source(Some(TrackId(3)));
        assert!(tap.wants_track(TrackId(3)) && !tap.wants_main());
        r.set_enabled(false);
        assert!(!tap.wants_track(TrackId(3)));
    }

    #[test]
    fn device_meter_publishes_only_while_read() {
        let m = DeviceMeter::default();
        assert!(!m.wanted());
        let mut out = [0.0f32; 8];
        assert_eq!(m.read(&mut out), 0);
        assert!(m.wanted());
        m.publish(&[1.0, 2.0, 3.0]);
        assert_eq!(m.read(&mut out), 3);
        assert_eq!(&out[..3], &[1.0, 2.0, 3.0]);
        for _ in 0..METER_INTEREST {
            assert!(m.wanted());
        }
        assert!(!m.wanted(), "interest runs out when nobody reads");
        // Longer than the slot: clamped, never out of bounds.
        m.publish(&[0.5; DEVICE_METER_LEN + 40]);
        let mut big = [0.0f32; DEVICE_METER_LEN + 40];
        assert_eq!(m.read(&mut big), DEVICE_METER_LEN);
    }
}
