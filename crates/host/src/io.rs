//! Per-bus scratch audio buffers shared by the VST3 and CLAP back ends.
//!
//! Allocated in `prepare`; the audio thread only copies into and out of
//! them. Channel pointer tables are built once (the boxed channel slices
//! never move), so building `ProcessData`/`clap_process` is allocation-free.

use aitrack_dsp::AudioBuffer;

pub(crate) struct BusScratch {
    buses: Vec<Vec<Box<[f32]>>>,
    ptrs: Vec<Vec<*mut f32>>,
    max_frames: usize,
}

// SAFETY: the raw pointers point into `buses`, which this struct owns; they
// move together and are only dereferenced by whoever holds `&mut self`.
unsafe impl Send for BusScratch {}

impl BusScratch {
    pub fn new(channels_per_bus: &[usize], max_frames: usize) -> Self {
        let mut buses: Vec<Vec<Box<[f32]>>> =
            channels_per_bus.iter().map(|&n| (0..n).map(|_| vec![0.0f32; max_frames.max(1)].into_boxed_slice()).collect()).collect();
        let ptrs = buses.iter_mut().map(|chs| chs.iter_mut().map(|c| c.as_mut_ptr()).collect()).collect();
        Self { buses, ptrs, max_frames }
    }

    pub fn bus_count(&self) -> usize {
        self.buses.len()
    }

    pub fn max_frames(&self) -> usize {
        self.max_frames
    }

    /// Pointer table for `bus` (stable for the lifetime of `self`).
    pub fn ptrs(&mut self, bus: usize) -> *mut *mut f32 {
        self.ptrs.get_mut(bus).map_or(std::ptr::null_mut(), |p| p.as_mut_ptr())
    }

    /// All buses (the slice length and boxes are fixed, so the pointer
    /// tables stay valid).
    pub fn buses_mut(&mut self) -> &mut [Vec<Box<[f32]>>] {
        &mut self.buses
    }

    /// Copy `src` into `bus`; channels beyond the source repeat its last
    /// channel (mono → stereo), a missing source clears the bus.
    pub fn fill_from(&mut self, bus: usize, src: Option<&AudioBuffer>, frames: usize) {
        let Some(chs) = self.buses.get_mut(bus) else { return };
        let frames = frames.min(self.max_frames);
        for (ch, dst) in chs.iter_mut().enumerate() {
            match src {
                Some(src) if src.channels() > 0 => {
                    let s = src.channel(ch.min(src.channels() - 1));
                    let n = frames.min(s.len());
                    dst[..n].copy_from_slice(&s[..n]);
                    dst[n..frames].fill(0.0);
                }
                _ => dst[..frames].fill(0.0),
            }
        }
    }

    pub fn clear(&mut self, bus: usize, frames: usize) {
        let frames = frames.min(self.max_frames);
        if let Some(chs) = self.buses.get_mut(bus) {
            for c in chs {
                c[..frames].fill(0.0);
            }
        }
    }

    /// Copy `bus` into `dst`. Extra destination channels repeat the bus's
    /// last channel; a bus with no channels clears `dst`.
    pub fn write_to(&self, bus: usize, dst: &mut AudioBuffer) {
        let frames = dst.frames().min(self.max_frames);
        let chs = self.buses.get(bus);
        for ch in 0..dst.channels() {
            let out = &mut dst.channel_mut(ch)[..frames];
            match chs {
                Some(chs) if !chs.is_empty() => out.copy_from_slice(&chs[ch.min(chs.len() - 1)][..frames]),
                _ => out.fill(0.0),
            }
        }
    }
}

/// Replace NaN/inf (a misbehaving plugin) with silence. Returns true if any
/// sample had to be fixed.
pub(crate) fn sanitize(buf: &mut AudioBuffer) -> bool {
    let mut bad = false;
    for ch in 0..buf.channels() {
        for s in buf.channel_mut(ch) {
            if !s.is_finite() {
                *s = 0.0;
                bad = true;
            }
        }
    }
    bad
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mono_bus_fans_out() {
        let mut s = BusScratch::new(&[1], 16);
        let mut src = AudioBuffer::new(2, 16);
        src.channel_mut(0).fill(0.5);
        s.fill_from(0, Some(&src), 16);
        let mut dst = AudioBuffer::new(2, 16);
        s.write_to(0, &mut dst);
        assert_eq!(dst.peak(0), 0.5);
        assert_eq!(dst.peak(1), 0.5);
    }

    #[test]
    fn sanitize_nan() {
        let mut b = AudioBuffer::new(1, 4);
        b.channel_mut(0)[2] = f32::NAN;
        assert!(sanitize(&mut b));
        assert_eq!(b.channel(0)[2], 0.0);
    }
}
