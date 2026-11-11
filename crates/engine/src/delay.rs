//! Fixed delay lines used for plugin delay compensation.

use aitrack_dsp::AudioBuffer;

/// A stereo (or N-channel) delay of a fixed number of samples, allocated
/// off the audio thread. A delay of 0 is a no-op.
#[derive(Clone, Debug)]
pub struct DelayLine {
    delay: usize,
    channels: usize,
    ring: Vec<f32>,
    len: usize,
    pos: usize,
}

impl DelayLine {
    pub fn new(channels: usize, delay: usize) -> Self {
        let len = if delay == 0 { 0 } else { delay + 1 };
        Self { delay, channels, ring: vec![0.0; len * channels], len, pos: 0 }
    }

    pub fn none() -> Self {
        Self::new(0, 0)
    }

    pub fn delay(&self) -> usize {
        self.delay
    }

    pub fn is_active(&self) -> bool {
        self.delay > 0
    }

    /// Delay `buf` in place. Real-time safe.
    pub fn process(&mut self, buf: &mut AudioBuffer) {
        if self.delay == 0 {
            return;
        }
        let frames = buf.frames();
        let chans = self.channels.min(buf.channels());
        let start = self.pos;
        for ch in 0..chans {
            let ring = &mut self.ring[ch * self.len..(ch + 1) * self.len];
            let mut pos = start;
            for s in buf.channel_mut(ch).iter_mut() {
                ring[pos] = *s;
                pos += 1;
                if pos == self.len {
                    pos = 0;
                }
                *s = ring[pos];
            }
        }
        self.pos = (start + frames) % self.len;
    }

    pub fn clear(&mut self) {
        self.ring.fill(0.0);
    }

    /// Keep the audio in flight when a graph is recompiled with the same
    /// delay (so edits do not drop samples). Real-time safe (pointer swap).
    pub fn adopt(&mut self, old: &mut DelayLine) {
        if self.delay == old.delay && self.channels == old.channels {
            std::mem::swap(self, old);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delays_across_blocks() {
        let mut d = DelayLine::new(2, 3);
        let mut b = AudioBuffer::new(2, 2);
        let mut out = Vec::new();
        for block in 0..4 {
            for i in 0..2 {
                b.channel_mut(0)[i] = (block * 2 + i + 1) as f32;
            }
            d.process(&mut b);
            out.extend_from_slice(b.channel(0));
        }
        assert_eq!(out, vec![0.0, 0.0, 0.0, 1.0, 2.0, 3.0, 4.0, 5.0]);
    }
}
