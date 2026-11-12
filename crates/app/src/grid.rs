//! The adaptive time grid shared by the arrangement, the piano roll and the
//! sample editor: it gets finer as the view zooms in, so off-beat positions
//! become reachable without picking a snap value by hand.

// Nothing draws or snaps with it yet; the views adopt it next.
#![allow(dead_code)]

use aitrack_model::{Tick, TimeSignature, PPQ};

/// Closest two grid lines may sit at medium density, in pixels.
const BASE_SPACING: f32 = 14.;
/// The finest straight step: a 1/128 note.
const FINEST: Tick = PPQ / 32;

/// The grid at one zoom level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridSpec {
    /// Distance between grid lines.
    pub step: Tick,
    pub bar: Tick,
    pub beat: Tick,
}

/// How strong a grid line is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Line {
    Bar,
    Beat,
    Sub,
}

/// The finest grid whose lines are at least about 14 px apart at this zoom
/// (`px_per_beat` is the width of a quarter note). `density` moves that
/// spacing like Live's Narrowest … Widest, -2..=2 with 0 = medium: each step
/// halves (negative) or doubles (positive) it. Steps run from 8 bars down to
/// the beat and then the beat halved down to 1/128; with `triplet` the steps
/// under the beat are thirds (1/8T, 1/16T, …).
pub fn adaptive(px_per_beat: f32, sig: TimeSignature, density: i8, triplet: bool) -> GridSpec {
    let bar = sig.bar_ticks().max(1);
    let beat = (PPQ * 4 / i64::from(sig.denominator.max(1))).max(1);
    let spacing = BASE_SPACING * 2f32.powi(i32::from(density.clamp(-2, 2)));
    let wide_enough = |step: Tick| step as f32 * px_per_beat / PPQ as f32 >= spacing;

    // Coarse to fine; every step is narrower than the one before.
    let mut step = bar * 8;
    for coarse in [bar * 4, bar * 2, bar, beat] {
        if !wide_enough(coarse) {
            return GridSpec { step, bar, beat };
        }
        step = coarse;
    }
    let mut straight = beat / 2;
    while straight >= FINEST {
        let fine = if triplet { straight * 2 / 3 } else { straight };
        if fine < 1 || !wide_enough(fine) {
            break;
        }
        step = fine;
        straight /= 2;
    }
    GridSpec { step, bar, beat }
}

impl GridSpec {
    /// How strong the line at `tick` is.
    pub fn line(&self, tick: Tick) -> Line {
        if tick.rem_euclid(self.bar) == 0 {
            Line::Bar
        } else if tick.rem_euclid(self.beat) == 0 {
            Line::Beat
        } else {
            Line::Sub
        }
    }

    /// The grid line at or before `t`.
    pub fn floor(&self, t: Tick) -> Tick {
        t.div_euclid(self.step) * self.step
    }

    /// The grid line nearest to `t` (halfway goes to the later one).
    pub fn round(&self, t: Tick) -> Tick {
        (t + self.step / 2).div_euclid(self.step) * self.step
    }

    /// The step as a note value or a number of bars: "1/16", "1/8T", "1 Bar", "4 Bars".
    pub fn label(&self) -> String {
        if self.step >= self.bar && self.step % self.bar == 0 {
            return match self.step / self.bar {
                1 => "1 Bar".into(),
                n => format!("{n} Bars"),
            };
        }
        let whole = PPQ * 4;
        if whole % self.step == 0 {
            let n = whole / self.step;
            if n.count_ones() == 1 {
                return format!("1/{n}");
            }
            // A triplet is two thirds of the straight note: 12 per whole note is 1/8T.
            if n * 2 % 3 == 0 && (n * 2 / 3).count_ones() == 1 {
                return format!("1/{}T", n * 2 / 3);
            }
        }
        format!("{} ticks", self.step)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FOUR_FOUR: TimeSignature = TimeSignature { numerator: 4, denominator: 4 };

    #[test]
    fn zooming_in_refines_the_grid() {
        for triplet in [false, true] {
            for density in -2..=2 {
                let mut last = Tick::MAX;
                let mut zoom = 0.05;
                while zoom < 20_000. {
                    let g = adaptive(zoom, FOUR_FOUR, density, triplet);
                    assert!(g.step <= last, "zoom {zoom}: {} after {last}", g.step);
                    assert!(g.step >= 1);
                    last = g.step;
                    zoom *= 1.1;
                }
                // From 8 bars all the way down to 1/128 (or its triplet).
                assert_eq!(adaptive(0.05, FOUR_FOUR, density, triplet).step, 32 * PPQ);
                assert_eq!(last, if triplet { 20 } else { 30 });
            }
        }
    }

    #[test]
    fn picks_the_finest_step_that_is_wide_enough() {
        // 56 px per beat: sixteenths are 14 px apart, thirty-seconds 7 px.
        assert_eq!(adaptive(56., FOUR_FOUR, 0, false).step, PPQ / 4);
        assert_eq!(adaptive(55., FOUR_FOUR, 0, false).step, PPQ / 2);
        // Narrower density allows closer lines, wider needs more room.
        assert_eq!(adaptive(56., FOUR_FOUR, -1, false).step, PPQ / 8);
        assert_eq!(adaptive(56., FOUR_FOUR, 1, false).step, PPQ / 2);
        assert_eq!(adaptive(56., FOUR_FOUR, 2, false).step, PPQ);
        // Triplets: 1/8T is 18.7 px here, 1/16T only 9.3 px.
        assert_eq!(adaptive(56., FOUR_FOUR, 0, true).step, PPQ / 3);
        // Zoomed out, the grid is whole bars.
        assert_eq!(adaptive(4., FOUR_FOUR, 0, false).step, 4 * PPQ);
        assert_eq!(adaptive(2., FOUR_FOUR, 0, false).step, 8 * PPQ);
    }

    #[test]
    fn bar_and_beat_follow_the_signature() {
        let waltz = adaptive(40., TimeSignature { numerator: 3, denominator: 4 }, 0, false);
        assert_eq!((waltz.bar, waltz.beat), (3 * PPQ, PPQ));
        let six_eight = adaptive(40., TimeSignature { numerator: 6, denominator: 8 }, 0, false);
        assert_eq!((six_eight.bar, six_eight.beat), (3 * PPQ, PPQ / 2));
        // In 6/8 the beat is an eighth, so the steps under it are sixteenths and finer.
        assert_eq!(adaptive(30., TimeSignature { numerator: 6, denominator: 8 }, 0, false).step, PPQ / 2);
        assert_eq!(six_eight.line(3 * PPQ), Line::Bar);
        assert_eq!(six_eight.line(PPQ / 2), Line::Beat);
        assert_eq!(six_eight.line(PPQ / 4), Line::Sub);
        assert_eq!(waltz.line(-3 * PPQ), Line::Bar);
        assert_eq!(waltz.line(-PPQ), Line::Beat);
    }

    #[test]
    fn floor_and_round_handle_negative_ticks() {
        let g = GridSpec { step: 240, bar: 4 * PPQ, beat: PPQ };
        assert_eq!(g.floor(250), 240);
        assert_eq!(g.floor(240), 240);
        assert_eq!(g.floor(-1), -240);
        assert_eq!(g.floor(-240), -240);
        assert_eq!(g.floor(-241), -480);
        assert_eq!(g.round(119), 0);
        assert_eq!(g.round(120), 240);
        assert_eq!(g.round(-119), 0);
        assert_eq!(g.round(-120), 0);
        assert_eq!(g.round(-121), -240);
        assert_eq!(g.round(-360), -240);
    }

    #[test]
    fn labels() {
        let label = |step| GridSpec { step, bar: 4 * PPQ, beat: PPQ }.label();
        assert_eq!(label(32 * PPQ), "8 Bars");
        assert_eq!(label(16 * PPQ), "4 Bars");
        assert_eq!(label(4 * PPQ), "1 Bar");
        assert_eq!(label(PPQ), "1/4");
        assert_eq!(label(PPQ / 4), "1/16");
        assert_eq!(label(PPQ / 32), "1/128");
        assert_eq!(label(PPQ / 3), "1/8T");
        assert_eq!(label(PPQ / 6), "1/16T");
        assert_eq!(label(20), "1/128T");
        // A 3/4 bar is not a note value.
        assert_eq!(GridSpec { step: 3 * PPQ, bar: 3 * PPQ, beat: PPQ }.label(), "1 Bar");
    }
}
