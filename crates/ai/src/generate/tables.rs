//! Every number that decides how a generated part feels, in one place.
//!
//! Almost all of it is ASSUMED, not measured: conventions from production
//! guides (see docs/research/22-generador-midi.md, sections 2.2 to 2.6),
//! meant to be tuned by ear. Change a value here and nothing else has to
//! move; the quality tests in `quality.rs` check the properties that must
//! survive any tuning (in key, in register, kick clear of the bass, velocity
//! not flat...).

use aitrack_model::Tick;

use super::grid::STEP;
use super::spec::{Feel, Genre, Role};

// ───────────────────────────── Register ─────────────────────────────

/// Range, polyphony and shortest note of a role.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub lo: u8,
    pub hi: u8,
    /// Most notes sounding at once.
    pub poly: usize,
    pub min_len: Tick,
}

pub fn limits(role: Role) -> Limits {
    let t32 = STEP / 2; // 1/32 note
    match role {
        Role::Sub => Limits { lo: 24, hi: 48, poly: 1, min_len: t32 },
        Role::Bass => Limits { lo: 28, hi: 55, poly: 1, min_len: t32 },
        Role::Chord => Limits { lo: 48, hi: 79, poly: 5, min_len: STEP },
        Role::Lead => Limits { lo: 60, hi: 88, poly: 1, min_len: t32 },
        Role::Arp => Limits { lo: 55, hi: 91, poly: 1, min_len: t32 },
        // Drum keys come from the pad map; one hit per pad at a time.
        _ => Limits { lo: 0, hi: 127, poly: 1, min_len: t32 },
    }
}

/// Top note of a chord voicing (the upper voice is the one you hear).
pub const CHORD_TOP: u8 = 76;
/// Lowest note of a chord voicing.
pub const CHORD_BOTTOM: u8 = 48;
/// Where chord voicings try to sit (centre of gravity, MIDI).
pub const CHORD_CENTRE: f32 = 62.;
/// Where a bass root is placed by default, and for 808s and subs.
pub const BASS_CENTRE: u8 = 38;
pub const SUB_CENTRE: u8 = 33;
/// Where a lead line is centred.
pub const LEAD_CENTRE: u8 = 72;
/// Gap left between a note and the next one in a mono part (ticks).
pub const MONO_GAP: Tick = 30;
/// Most passing/blues notes a role may declare, as a part of its notes.
pub const MAX_PASSING: f32 = 0.10;
/// Where the bass lifts off before the next kick (ticks), under `Avoid`.
pub const KICK_CLEARANCE: Tick = 20;

// ───────────────────────────── Velocity ─────────────────────────────

/// Accent class 0..=4 as a MIDI velocity range (of 127):
/// ghost, soft, normal, accent, peak.
pub const ACCENT_VELOCITY: [(f32, f32); 5] = [(18., 35.), (42., 62.), (70., 90.), (98., 115.), (118., 127.)];

/// Random spread added to a velocity, as a fraction of its value.
pub fn velocity_jitter(role: Role) -> f32 {
    match role {
        Role::Kick => 0.01,
        Role::Hat | Role::Perc => 0.15,
        Role::OpenHat => 0.08,
        Role::Snare | Role::Clap => 0.04,
        _ => 0.05,
    }
}

/// An extra lift for the top voice of a chord so the line on top is heard.
pub const TOP_VOICE_LIFT: f32 = 1.04;

// ───────────────────────────── Groove ─────────────────────────────

/// Linn swing percentage of each feel label: the part of a pair of sixteenths
/// that the first one takes. 50 is straight, 66.7 is a triplet.
pub const FEEL_SWING: [(Feel, f32); 5] =
    [(Feel::Straight, 0.50), (Feel::LightSwing, 0.54), (Feel::Swing, 0.58), (Feel::Shuffle, 0.66), (Feel::Loose, 0.52)];

/// A micro-timing offset and its random spread, in milliseconds.
#[derive(Clone, Copy, Debug, Default)]
pub struct Micro {
    pub offset_ms: f32,
    pub jitter_ms: f32,
}

/// Everything that differs by genre.
#[derive(Clone, Copy, Debug)]
pub struct GenreRow {
    pub genre: Genre,
    /// How much of a feel's swing the genre takes (techno: little).
    pub swing_scale: f32,
    /// The feel the templates choose when the request does not say.
    pub feel: Feel,
    pub kick_policy: KickPolicy,
    pub kick_ms: f32,
    pub snare_ms: f32,
    /// Strum between chord voices, bottom to top.
    pub strum_ms: f32,
    /// Voices of a chord voicing.
    pub voices: usize,
    /// Genres that play ghost notes.
    pub ghosts: bool,
    /// Compact voicings (house) or open ones (pads).
    pub compact: bool,
    pub chord_gate: super::spec::Gate,
}

/// What the bass does around the kick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KickPolicy {
    /// Never start on a kick step.
    Avoid,
    /// Start together with the kick (808 style).
    Lock,
    Free,
}

use super::spec::Gate;
use KickPolicy::{Avoid, Free, Lock};

pub const GENRES: [GenreRow; 12] = [
    GenreRow { genre: Genre::House, swing_scale: 1.0, feel: Feel::LightSwing, kick_policy: Avoid, kick_ms: 0., snare_ms: 0., strum_ms: 4., voices: 4, ghosts: false, compact: true, chord_gate: Gate::Mid },
    GenreRow { genre: Genre::DeepHouse, swing_scale: 1.2, feel: Feel::LightSwing, kick_policy: Avoid, kick_ms: 0., snare_ms: 2., strum_ms: 8., voices: 4, ghosts: false, compact: true, chord_gate: Gate::Mid },
    GenreRow { genre: Genre::TechHouse, swing_scale: 1.0, feel: Feel::LightSwing, kick_policy: Avoid, kick_ms: 0., snare_ms: 0., strum_ms: 0., voices: 3, ghosts: false, compact: true, chord_gate: Gate::Short },
    GenreRow { genre: Genre::Techno, swing_scale: 0.4, feel: Feel::Straight, kick_policy: Avoid, kick_ms: -2., snare_ms: 0., strum_ms: 0., voices: 3, ghosts: false, compact: true, chord_gate: Gate::Short },
    GenreRow { genre: Genre::Trance, swing_scale: 0.4, feel: Feel::Straight, kick_policy: Avoid, kick_ms: 0., snare_ms: 0., strum_ms: 0., voices: 4, ghosts: false, compact: false, chord_gate: Gate::Mid },
    GenreRow { genre: Genre::Dnb, swing_scale: 0.5, feel: Feel::Straight, kick_policy: Free, kick_ms: 0., snare_ms: 0., strum_ms: 0., voices: 4, ghosts: true, compact: false, chord_gate: Gate::Long },
    GenreRow { genre: Genre::Dubstep, swing_scale: 0.4, feel: Feel::Straight, kick_policy: Lock, kick_ms: 0., snare_ms: 4., strum_ms: 0., voices: 3, ghosts: false, compact: false, chord_gate: Gate::Long },
    GenreRow { genre: Genre::Trap, swing_scale: 0.3, feel: Feel::Straight, kick_policy: Lock, kick_ms: 0., snare_ms: 4., strum_ms: 0., voices: 4, ghosts: true, compact: false, chord_gate: Gate::Long },
    GenreRow { genre: Genre::UkGarage, swing_scale: 1.5, feel: Feel::Swing, kick_policy: Free, kick_ms: 0., snare_ms: 3., strum_ms: 6., voices: 4, ghosts: true, compact: true, chord_gate: Gate::Short },
    GenreRow { genre: Genre::Reggaeton, swing_scale: 0.8, feel: Feel::LightSwing, kick_policy: Free, kick_ms: 0., snare_ms: 0., strum_ms: 0., voices: 4, ghosts: false, compact: true, chord_gate: Gate::Mid },
    GenreRow { genre: Genre::Pop, swing_scale: 1.0, feel: Feel::Straight, kick_policy: Free, kick_ms: 0., snare_ms: 0., strum_ms: 0., voices: 4, ghosts: false, compact: false, chord_gate: Gate::Long },
    GenreRow { genre: Genre::Other, swing_scale: 1.0, feel: Feel::LightSwing, kick_policy: Avoid, kick_ms: 0., snare_ms: 0., strum_ms: 4., voices: 4, ghosts: false, compact: true, chord_gate: Gate::Mid },
];

pub fn row(genre: Genre) -> &'static GenreRow {
    let g = genre.style();
    GENRES.iter().find(|r| r.genre == g).unwrap_or(&GENRES[0])
}

/// The Linn swing fraction (0.5 = straight) for a feel in a genre.
pub fn swing(genre: Genre, feel: Feel) -> f32 {
    let base = FEEL_SWING.iter().find(|(f, _)| *f == feel).map_or(0.5, |(_, s)| *s);
    0.5 + (base - 0.5) * row(genre).swing_scale
}

/// The roles a feel swings. Kick, snare and clap stay straight in every feel.
pub fn swung(feel: Feel, role: Role) -> bool {
    match feel {
        Feel::Straight => false,
        Feel::LightSwing | Feel::Swing | Feel::Loose => matches!(role, Role::Hat | Role::OpenHat | Role::Perc | Role::Arp),
        Feel::Shuffle => matches!(role, Role::Hat | Role::OpenHat | Role::Perc | Role::Arp | Role::Bass | Role::Sub | Role::Chord | Role::Lead),
    }
}

/// Micro-timing of a role in a genre (offset, spread) in milliseconds.
pub fn micro(genre: Genre, role: Role) -> Micro {
    let g = row(genre);
    match role {
        Role::Kick => Micro { offset_ms: g.kick_ms, jitter_ms: 1. },
        Role::Snare | Role::Clap => Micro { offset_ms: g.snare_ms, jitter_ms: 1.5 },
        Role::Hat | Role::OpenHat => Micro { offset_ms: 0., jitter_ms: 4. },
        Role::Perc => Micro { offset_ms: 0., jitter_ms: 3. },
        Role::Crash => Micro { offset_ms: 0., jitter_ms: 1. },
        Role::Bass | Role::Sub => Micro { offset_ms: 0., jitter_ms: 1. },
        Role::Chord | Role::Lead | Role::Arp => Micro { offset_ms: 0., jitter_ms: 2. },
    }
}

/// The largest micro-timing shift (ms) any role can get, offset plus spread;
/// the tests check against it.
pub fn micro_bound_ms(genre: Genre, role: Role) -> f32 {
    let m = micro(genre, role);
    m.offset_ms.abs() + m.jitter_ms
}

/// The most a chord strum can add (ms), spread over its voices.
pub fn strum_bound_ms(genre: Genre) -> f32 {
    row(genre).strum_ms
}

// ───────────────────────────── Density ─────────────────────────────

/// Notes per bar a role normally plays (min, max), used by the soft checks.
pub fn density_per_bar(role: Role) -> (usize, usize) {
    match role {
        Role::Bass | Role::Sub => (1, 16),
        Role::Chord => (3, 20),
        Role::Arp => (4, 16),
        Role::Lead => (1, 16),
        Role::Kick => (1, 8),
        Role::Hat => (2, 32),
        _ => (0, 24),
    }
}

/// Soft limits for a good result; a result outside them is reported.
pub const TOP_VOICE_JUMP_WARN: i32 = 5;
pub const TOP_VOICE_JUMP_MAX: i32 = 7;
pub const MELODY_MAX_RANGE: i32 = 14;
pub const MELODY_STEPWISE_MIN: f32 = 0.6;
/// A repair rate above this share of the notes means the spec was bad.
pub const MAX_FIX_RATE: f32 = 0.25;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_genre_has_a_row() {
        for g in Genre::ALL {
            assert_eq!(row(g).genre, g.style());
        }
    }

    #[test]
    fn swing_follows_the_linn_convention() {
        assert_eq!(swing(Genre::House, Feel::Straight), 0.5);
        assert!((swing(Genre::House, Feel::LightSwing) - 0.54).abs() < 1e-6);
        assert!((swing(Genre::House, Feel::Shuffle) - 0.66).abs() < 1e-6);
        assert!(swing(Genre::Techno, Feel::Swing) < swing(Genre::House, Feel::Swing));
        assert!(swing(Genre::UkGarage, Feel::Swing) > swing(Genre::House, Feel::Swing));
    }

    #[test]
    fn kick_and_snare_never_swing() {
        for f in [Feel::LightSwing, Feel::Swing, Feel::Shuffle, Feel::Loose] {
            assert!(![Role::Kick, Role::Snare, Role::Clap, Role::Crash].iter().any(|r| swung(f, *r)));
        }
        assert!(swung(Feel::Swing, Role::Hat));
        assert!(!swung(Feel::Straight, Role::Hat));
    }

    #[test]
    fn accent_ranges_rise() {
        assert!(ACCENT_VELOCITY.windows(2).all(|w| w[0].1 < w[1].0));
        assert!(ACCENT_VELOCITY[4].1 <= 127.);
    }
}
