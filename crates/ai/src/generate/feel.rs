//! Feel: swing, micro-timing and velocity, applied in a fixed order (swing,
//! micro-timing, velocity, gate). Swing uses the Linn convention: the number
//! is the part of a pair of sixteenths that the first one takes, so 50 % is
//! straight and the second sixteenth is delayed by `(s - 0.5) * 480` ticks.

use aitrack_model::Tick;

use super::grid::{STEP, ms_to_ticks};
use super::rng::Rng;
use super::spec::{Feel, Genre, Role};
use super::tables::{self, ACCENT_VELOCITY};

/// Ticks a swung second sixteenth is delayed at Linn swing `s`.
pub fn swing_delay(s: f32) -> Tick {
    ((s - 0.5) * (2 * STEP) as f32).round() as Tick
}

/// How a part is timed.
#[derive(Clone, Copy, Debug)]
pub struct Timing {
    pub genre: Genre,
    pub feel: Feel,
    pub bpm: f64,
    /// Micro-timing on. Swing stays in either case.
    pub humanize: bool,
}

impl Timing {
    pub fn swing(&self) -> f32 {
        tables::swing(self.genre, self.feel)
    }

    /// The start of a note at grid position `nominal` (step `in_bar` of its bar).
    pub fn start(&self, nominal: Tick, in_bar: u32, role: Role, rng: &mut Rng) -> Tick {
        let mut t = nominal;
        if in_bar % 2 == 1 && tables::swung(self.feel, role) {
            t += swing_delay(self.swing());
        }
        if self.humanize {
            t += self.micro(role, rng);
        }
        t.max(0)
    }

    /// Micro-timing of a role in ticks: its offset plus a random spread.
    pub fn micro(&self, role: Role, rng: &mut Rng) -> Tick {
        let m = tables::micro(self.genre, role);
        let loose = if self.feel == Feel::Loose { 2. } else { 1. };
        ms_to_ticks(m.offset_ms + m.jitter_ms * loose * rng.signed(), self.bpm)
    }

    /// The largest shift micro-timing can add to a note, in ticks.
    pub fn micro_bound(&self, role: Role) -> Tick {
        let loose = if self.feel == Feel::Loose { 2. } else { 1. };
        let m = tables::micro(self.genre, role);
        ms_to_ticks(m.offset_ms.abs() + m.jitter_ms * loose, self.bpm) + 1
    }

    /// The time between chord voices, bottom to top, as ticks per voice.
    pub fn strum(&self, voices: usize) -> Tick {
        if !self.humanize || voices < 2 {
            return 0;
        }
        ms_to_ticks(tables::strum_bound_ms(self.genre), self.bpm) / (voices - 1) as Tick
    }
}

/// Velocity (0..1) of a hit of accent class `accent`: a value inside the
/// class's range plus the role's jitter.
pub fn velocity(role: Role, accent: u8, rng: &mut Rng) -> f32 {
    let (lo, hi) = ACCENT_VELOCITY[accent.min(4) as usize];
    let base = rng.between(lo, hi) / 127.;
    (base * (1. + tables::velocity_jitter(role) * rng.signed())).clamp(0.02, 1.)
}

/// A step string character as an accent class (`x` is a normal hit).
pub fn accent_of(c: char) -> Option<u8> {
    match c {
        '.' => None,
        'x' => Some(2),
        d => d.to_digit(10).map(|d| d.min(4) as u8),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timing(feel: Feel, humanize: bool) -> Timing {
        Timing { genre: Genre::House, feel, bpm: 124., humanize }
    }

    #[test]
    fn swing_delays_follow_the_linn_formula() {
        // 54 % -> 19 ticks, 58 % -> 38, 62 % -> 58, 66 % -> 77 (docs section 2.2).
        assert_eq!([0.54, 0.58, 0.62, 0.66].map(swing_delay), [19, 38, 58, 77]);
        assert_eq!(swing_delay(0.5), 0);
    }

    #[test]
    fn only_odd_steps_of_swung_roles_move() {
        let t = timing(Feel::Swing, false);
        let mut r = Rng::new(1);
        assert_eq!(t.start(1000, 2, Role::Hat, &mut r), 1000);
        assert_eq!(t.start(1000, 3, Role::Hat, &mut r), 1000 + swing_delay(t.swing()));
        assert_eq!(t.start(1000, 3, Role::Kick, &mut r), 1000);
        assert_eq!(t.start(1000, 3, Role::Clap, &mut r), 1000);
        assert_eq!(timing(Feel::Straight, false).start(1000, 3, Role::Hat, &mut r), 1000);
    }

    #[test]
    fn micro_timing_stays_inside_its_table() {
        let t = timing(Feel::Straight, true);
        let mut r = Rng::new(5);
        for role in [Role::Kick, Role::Hat, Role::Snare, Role::Bass] {
            let bound = t.micro_bound(role);
            assert!((0..500).all(|_| t.micro(role, &mut r).abs() <= bound), "{role:?}");
        }
        assert_eq!(timing(Feel::Straight, false).strum(4), 0);
        assert!(t.strum(4) * 3 <= ms_to_ticks(tables::strum_bound_ms(Genre::House), 124.));
    }

    #[test]
    fn velocities_follow_their_accent_class() {
        let mut r = Rng::new(2);
        for accent in 0..5u8 {
            let (lo, hi) = ACCENT_VELOCITY[accent as usize];
            let jitter = tables::velocity_jitter(Role::Snare);
            for _ in 0..200 {
                let v = velocity(Role::Snare, accent, &mut r) * 127.;
                assert!(v >= lo * (1. - jitter) - 0.01 && v <= (hi * (1. + jitter)).min(127.) + 0.01, "{accent} {v}");
            }
        }
        let kicks: Vec<f32> = (0..200).map(|_| velocity(Role::Kick, 4, &mut r)).collect();
        let mean = kicks.iter().sum::<f32>() / 200.;
        let sd = (kicks.iter().map(|k| (k - mean).powi(2)).sum::<f32>() / 200.).sqrt();
        assert!(sd < 0.03, "{sd}");
    }

    #[test]
    fn accent_chars() {
        assert_eq!((accent_of('.'), accent_of('x'), accent_of('0'), accent_of('4'), accent_of('9')), (None, Some(2), Some(0), Some(4), Some(4)));
    }
}
