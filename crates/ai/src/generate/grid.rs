//! The time grid of a clip: 1/16 steps in ticks, bars, and the millisecond
//! to tick conversion the feel uses.

use aitrack_model::{PPQ, Tick, TimeSignature};

/// One step is a sixteenth note.
pub const STEP: Tick = PPQ / 4;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Grid {
    pub bar_ticks: Tick,
    /// Steps in one bar: 16 in 4/4, 12 in 3/4 or 6/8, 14 in 7/8.
    pub steps_per_bar: u32,
    pub bars: u32,
}

impl Grid {
    pub fn new(signature: TimeSignature, bars: u32) -> Self {
        let bar_ticks = signature.bar_ticks().max(STEP);
        Self { bar_ticks, steps_per_bar: (bar_ticks / STEP).max(1) as u32, bars: bars.max(1) }
    }

    /// Length of the whole clip.
    pub fn clip_ticks(&self) -> Tick {
        self.bar_ticks * self.bars as Tick
    }

    pub fn total_steps(&self) -> u32 {
        self.steps_per_bar * self.bars
    }

    /// The tick of global step `step`.
    pub fn tick(&self, step: u32) -> Tick {
        step as Tick * STEP
    }

    pub fn bar_of(&self, tick: Tick) -> u32 {
        (tick.max(0) / self.bar_ticks) as u32
    }

    /// Step within its bar of the global step `step`.
    pub fn in_bar(&self, step: u32) -> u32 {
        step % self.steps_per_bar
    }

    /// Whether this is the common 4/4 grid the genre drum patterns assume.
    pub fn is_four_four(&self) -> bool {
        self.steps_per_bar == 16
    }
}

/// Milliseconds as ticks at `bpm`: 1 ms is `bpm × 0.016` ticks (960 PPQ).
pub fn ms_to_ticks(ms: f32, bpm: f64) -> Tick {
    (ms as f64 * bpm * PPQ as f64 / 60_000.).round() as Tick
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn four_four_has_sixteen_steps() {
        let g = Grid::new(TimeSignature::default(), 2);
        assert_eq!((g.bar_ticks, g.steps_per_bar, g.total_steps(), g.clip_ticks()), (3840, 16, 32, 7680));
        assert!(g.is_four_four());
        assert_eq!(g.tick(5), 1200);
        assert_eq!(g.bar_of(3840), 1);
    }

    #[test]
    fn other_meters_count_their_own_steps() {
        let six_eight = Grid::new(TimeSignature { numerator: 6, denominator: 8 }, 1);
        assert_eq!(six_eight.steps_per_bar, 12);
        let seven_eight = Grid::new(TimeSignature { numerator: 7, denominator: 8 }, 1);
        assert_eq!((seven_eight.steps_per_bar, seven_eight.bar_ticks), (14, 3360));
        assert!(!seven_eight.is_four_four());
    }

    #[test]
    fn milliseconds_follow_the_tempo() {
        assert_eq!(ms_to_ticks(1., 124.), 2);
        assert_eq!(ms_to_ticks(10., 124.), 20);
        assert_eq!(ms_to_ticks(10., 60.), 10);
        assert_eq!(ms_to_ticks(-5., 120.), -10);
    }
}
