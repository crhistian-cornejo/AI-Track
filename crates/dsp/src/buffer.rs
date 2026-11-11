//! Planar audio buffer with fixed capacity, allocated in `prepare`.

#[derive(Clone, Debug)]
pub struct AudioBuffer {
    data: Vec<f32>,
    channels: usize,
    capacity: usize,
    frames: usize,
}

impl AudioBuffer {
    /// Allocate `channels × capacity` samples (call off the audio thread).
    pub fn new(channels: usize, capacity: usize) -> Self {
        Self { data: vec![0.0; channels * capacity], channels, capacity, frames: capacity }
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    pub fn frames(&self) -> usize {
        self.frames
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Set the active block length (≤ capacity). Real-time safe.
    pub fn set_frames(&mut self, frames: usize) {
        debug_assert!(frames <= self.capacity, "block larger than prepared capacity");
        self.frames = frames.min(self.capacity);
    }

    pub fn channel(&self, ch: usize) -> &[f32] {
        let start = ch * self.capacity;
        &self.data[start..start + self.frames]
    }

    pub fn channel_mut(&mut self, ch: usize) -> &mut [f32] {
        let start = ch * self.capacity;
        &mut self.data[start..start + self.frames]
    }

    /// Two distinct channels mutably (stereo processing).
    pub fn stereo_mut(&mut self) -> (&mut [f32], &mut [f32]) {
        assert!(self.channels >= 2);
        let (l, r) = self.data.split_at_mut(self.capacity);
        (&mut l[..self.frames], &mut r[..self.frames])
    }

    pub fn clear(&mut self) {
        for ch in 0..self.channels {
            self.channel_mut(ch).fill(0.0);
        }
    }

    /// `self += other * gain`, channel by channel (mono sources fan out).
    pub fn add_from(&mut self, other: &AudioBuffer, gain: f32) {
        let frames = self.frames.min(other.frames);
        for ch in 0..self.channels {
            let src_ch = if other.channels == 0 { break } else { ch.min(other.channels - 1) };
            let src = &other.data[src_ch * other.capacity..src_ch * other.capacity + frames];
            let start = ch * self.capacity;
            for (d, s) in self.data[start..start + frames].iter_mut().zip(src) {
                *d += s * gain;
            }
        }
    }

    pub fn copy_from(&mut self, other: &AudioBuffer) {
        self.clear();
        self.add_from(other, 1.0);
    }

    pub fn apply_gain(&mut self, gain: f32) {
        for ch in 0..self.channels {
            for s in self.channel_mut(ch) {
                *s *= gain;
            }
        }
    }

    /// Peak absolute value per channel (for meters).
    pub fn peak(&self, ch: usize) -> f32 {
        self.channel(ch).iter().fold(0.0f32, |m, s| m.max(s.abs()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mix_and_peak() {
        let mut a = AudioBuffer::new(2, 64);
        let mut b = AudioBuffer::new(1, 64);
        b.set_frames(32);
        a.set_frames(32);
        b.channel_mut(0).fill(0.5);
        a.add_from(&b, 2.0);
        assert_eq!(a.peak(0), 1.0);
        assert_eq!(a.peak(1), 1.0);
        assert_eq!(a.channel(0).len(), 32);
    }
}
