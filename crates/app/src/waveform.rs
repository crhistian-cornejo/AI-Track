//! Waveform peaks for drawing audio clips (filled in by the sample-editor worker).

#[derive(Default)]
pub struct WaveformCache {}

impl WaveformCache {
    /// Housekeeping tick: true when new peaks arrived and the UI should repaint.
    pub fn poll(&mut self) -> bool {
        false
    }
}
