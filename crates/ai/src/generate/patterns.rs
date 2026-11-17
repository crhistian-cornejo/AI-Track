//! The catalog of rhythms: step strings with accent classes for every layer
//! a template can use. One bar of 4/4, 16 steps; `.` is silence, a digit is
//! a hit with its accent class (0 ghost .. 4 peak). Like `tables.rs`, the
//! positions are conventions to be tuned by ear (docs/research/22 section
//! 2.2), kept here as data.

use super::context::GenContext;
use super::grid::Grid;
use super::rng::Rng;
use super::spec::{Energy, Gate, Genre, Layer, Pattern, Role};

/// A rhythm for one bar: steps and accents, each `str` 16 chars long.
#[derive(Clone, Copy, Debug)]
pub struct Lane {
    pub steps: &'static str,
    pub accents: &'static str,
}

const fn lane(steps: &'static str, accents: &'static str) -> Lane {
    Lane { steps, accents }
}

// ───────────────────────────── Chords ─────────────────────────────

pub fn chord_lane(p: Pattern) -> Lane {
    match p {
        Pattern::StabsOffbeat => lane("..x...x...x...x.", "..3...2...3...2."),
        Pattern::Sync332 => lane("x..x..x.x..x..x.", "3..2..2.3..2..2."),
        Pattern::Charleston => lane("x.....x.........", "3.....2........."),
        Pattern::Pump8ths => lane("x.x.x.x.x.x.x.x.", "3.2.2.2.3.2.2.2."),
        _ => lane("x...............", "3..............."),
    }
}

/// The chord rhythm a genre likes, by how busy the request is.
pub fn chord_pattern(genre: Genre, density: f32, rng: &mut Rng) -> Pattern {
    let pool: &[Pattern] = match genre.style() {
        Genre::DeepHouse => &[Pattern::StabsOffbeat, Pattern::Sync332, Pattern::Charleston, Pattern::Whole],
        Genre::House | Genre::Pop => &[Pattern::StabsOffbeat, Pattern::Sync332, Pattern::Pump8ths, Pattern::Whole],
        Genre::TechHouse => &[Pattern::StabsOffbeat, Pattern::Sync332, Pattern::Charleston],
        Genre::Techno => &[Pattern::StabsOffbeat, Pattern::Charleston, Pattern::Whole],
        Genre::Trance => &[Pattern::Pump8ths, Pattern::Whole, Pattern::StabsOffbeat],
        Genre::UkGarage => &[Pattern::Sync332, Pattern::Charleston, Pattern::StabsOffbeat],
        Genre::Reggaeton => &[Pattern::Sync332, Pattern::Charleston],
        _ => &[Pattern::Whole, Pattern::Charleston, Pattern::Sync332],
    };
    if density < 0.2 {
        return Pattern::Whole;
    }
    *rng.pick(pool).unwrap_or(&Pattern::Whole)
}

// ───────────────────────────── Bass ─────────────────────────────

/// The steps (one bar) of a named bass pattern. `kick` marks the kick steps
/// of the bar when known; rolling bass leaves them free.
pub fn bass_lane(p: Pattern, kick: &[bool], density: f32, rng: &mut Rng) -> (String, String) {
    let fixed = |l: Lane| (l.steps.to_owned(), l.accents.to_owned());
    match p {
        Pattern::Offbeat => fixed(lane("..x...x...x...x.", "..3...3...3...3.")),
        Pattern::Octaves => fixed(lane("..xx..xx..xx..xx", "..32..32..32..32")),
        Pattern::RootFifth => fixed(lane("..x...x...x...x.", "..3...2...3...2.")),
        Pattern::Whole => fixed(lane("x...............", "3...............")),
        Pattern::Sync332 => fixed(lane("x..x..x.x..x..x.", "4..2..3.4..2..3.")),
        Pattern::Sub808 => {
            // On the kick, plus a syncopation or two.
            let mut steps = vec!['.'; 16];
            let mut acc = vec!['.'; 16];
            for s in 0..16 {
                if kick.get(s).copied().unwrap_or(s == 0) {
                    steps[s] = 'x';
                    acc[s] = if s == 0 { '4' } else { '3' };
                }
            }
            let extra = 1 + (density > 0.6) as usize;
            for _ in 0..extra {
                let s = [3usize, 7, 11, 14][rng.below(4)];
                if steps[s] == '.' && !kick.get(s).copied().unwrap_or(false) && rng.chance(0.7) {
                    steps[s] = 'x';
                    acc[s] = '2';
                }
            }
            (steps.into_iter().collect(), acc.into_iter().collect())
        }
        // Rolling: everything except the first sixteenth of each beat (the kick).
        _ => {
            let mut steps = String::new();
            let mut acc = String::new();
            for s in 0..16usize {
                let on_kick = kick.get(s).copied().unwrap_or(s % 4 == 0);
                let offbeat = s % 4 == 2;
                let keep = !on_kick && (offbeat || rng.chance(1. - (1. - density) * 0.35));
                steps.push(if keep { 'x' } else { '.' });
                acc.push(if !keep {
                    '.'
                } else {
                    match s % 4 {
                        2 => '3',
                        1 => '1',
                        _ => '2',
                    }
                });
            }
            (steps, acc)
        }
    }
}

/// The chord-relative pitches that go with a bass pattern.
pub fn bass_pitches(p: Pattern) -> Vec<String> {
    let v: &[&str] = match p {
        Pattern::Octaves => &["r", "8", "r", "8"],
        Pattern::RootFifth => &["r", "r", "5", "r"],
        Pattern::Sync332 => &["r", "r", "5", "r", "7", "r"],
        _ => &["r"],
    };
    v.iter().map(|s| s.to_string()).collect()
}

/// The bass pattern a genre starts from.
pub fn bass_pattern(genre: Genre, rng: &mut Rng) -> Pattern {
    let pool: &[Pattern] = match genre.style() {
        Genre::Techno | Genre::TechHouse => &[Pattern::Rolling, Pattern::Rolling, Pattern::Offbeat],
        Genre::House => &[Pattern::Offbeat, Pattern::RootFifth, Pattern::Rolling],
        Genre::DeepHouse => &[Pattern::RootFifth, Pattern::Offbeat, Pattern::Sync332],
        Genre::Trance => &[Pattern::Octaves, Pattern::Offbeat],
        Genre::Dnb => &[Pattern::Sync332, Pattern::Whole],
        Genre::Dubstep | Genre::Trap => &[Pattern::Sub808],
        Genre::UkGarage | Genre::Reggaeton => &[Pattern::Sync332, Pattern::RootFifth],
        _ => &[Pattern::RootFifth, Pattern::Offbeat],
    };
    *rng.pick(pool).unwrap_or(&Pattern::Rolling)
}

// ───────────────────────────── Drums ─────────────────────────────

fn blank() -> Vec<char> {
    vec!['.'; 16]
}

fn put(v: &mut [char], step: usize, acc: u32) {
    if let Some(c) = v.get_mut(step) {
        *c = char::from_digit(acc.min(4), 10).unwrap_or('2');
    }
}

fn layer(role: Role, steps: &[char], acc: &[char]) -> Layer {
    let s: String = steps.iter().map(|&c| if c == '.' { '.' } else { 'x' }).collect();
    let a: String = acc.iter().collect();
    Layer { role, pattern: Pattern::Custom, steps: s, accents: a, gate: Gate::Short, ..Default::default() }
}

/// A lane from hits (step, accent).
fn from_hits(role: Role, hits: &[(usize, u32)]) -> Layer {
    let mut v = blank();
    for &(s, a) in hits {
        put(&mut v, s, a);
    }
    layer(role, &v, &v)
}

/// Hats: the main hits always, the weak sixteenths by chance.
fn hats(main: &[(usize, u32)], weak: &[usize], weak_acc: u32, chance: f32, ghost: Option<(usize, f32)>, rng: &mut Rng) -> Layer {
    let mut v = blank();
    for &s in weak {
        if rng.chance(chance) {
            put(&mut v, s, weak_acc);
        }
    }
    if let Some((s, p)) = ghost
        && v[s] == '.'
        && rng.chance(p)
    {
        put(&mut v, s, 0);
    }
    for &(s, a) in main {
        put(&mut v, s, a);
    }
    // A bar of hats is never nearly empty, however sparse the request.
    let mut guard = 0;
    while v.iter().filter(|c| **c != '.').count() < 6 && guard < 40 {
        guard += 1;
        let s = ODD[rng.below(ODD.len())];
        if v[s] == '.' {
            put(&mut v, s, weak_acc);
        }
    }
    // One ghost hat in every bar keeps the dynamics from going flat.
    if !v.contains(&'0') {
        let weak_hits: Vec<usize> = ODD.iter().copied().filter(|s| v[*s] == char::from_digit(weak_acc, 10).unwrap_or('1')).collect();
        if !weak_hits.is_empty() {
            let s = weak_hits[rng.below(weak_hits.len())];
            put(&mut v, s, 0);
        }
    }
    layer(Role::Hat, &v, &v)
}

/// Eighth notes: strong on beats 1 and 3, softer on 2 and 4, soft in between.
const EIGHTHS: [(usize, u32); 8] = [(0, 3), (2, 1), (4, 2), (6, 1), (8, 3), (10, 1), (12, 2), (14, 1)];
const ODD: [usize; 8] = [1, 3, 5, 7, 9, 11, 13, 15];
const SIXTEENTHS_OFF_KICK: [usize; 12] = [1, 2, 3, 5, 6, 7, 9, 10, 11, 13, 14, 15];

/// The layers of a drum pattern for a genre, with a few seeded choices
/// (which kick variation, which weak hats, which ghost notes). Four on the
/// floor and the backbeat never move; the rest does.
pub fn drum_layers(genre: Genre, density: f32, energy: Energy, rng: &mut Rng) -> Vec<Layer> {
    let four = [(0, 4), (4, 4), (8, 4), (12, 4)];
    let weak = 0.15 + 0.6 * density;
    let hi = energy == Energy::High;
    match genre.style() {
        Genre::DeepHouse => vec![
            from_hits(Role::Kick, &four),
            from_hits(Role::Clap, &[(4, 3), (12, 3)]),
            hats(&[(2, 2), (10, 2)], &ODD, 1, weak * 0.7, Some((15, 0.5)), rng),
            from_hits(Role::OpenHat, &[(6, 2), (14, 2)]),
        ],
        Genre::TechHouse => {
            let rim = [7usize, 11, 15][rng.below(3)];
            vec![
                from_hits(Role::Kick, &four),
                from_hits(Role::Clap, &[(4, 3), (12, 3)]),
                hats(&[(2, 3), (6, 3), (10, 3), (14, 3)], &ODD, 1, weak + 0.2, Some((13, 0.5)), rng),
                from_hits(Role::Perc, &[(rim, 2)]),
            ]
        }
        Genre::Techno => {
            let mut open = vec![(14usize, 2u32)];
            if hi {
                open.push((6, 2));
            }
            vec![
                from_hits(Role::Kick, &four),
                from_hits(Role::Perc, &[(4, 2), (12, 2)]),
                hats(&[(2, 2), (6, 2), (10, 2), (14, 2)], &ODD.iter().chain(&[0, 4, 8, 12]).copied().collect::<Vec<_>>(), 1, 0.85, None, rng),
                from_hits(Role::OpenHat, &open),
            ]
        }
        Genre::Trance => vec![
            from_hits(Role::Kick, &four),
            from_hits(Role::Clap, &[(4, 3), (12, 3)]),
            hats(&[(3, 2), (7, 2), (11, 2), (15, 2)], &[1, 5, 9, 13], 1, 0.9, None, rng),
            from_hits(Role::OpenHat, &[(2, 3), (6, 3), (10, 3), (14, 3)]),
        ],
        Genre::Dnb => {
            let kick: &[(usize, u32)] = if rng.chance(0.5) { &[(0, 4), (10, 4)] } else { &[(0, 4), (7, 4), (10, 4)] };
            let mut snare = vec![(4usize, 4u32), (12, 4)];
            for s in [2usize, 6, 14, 9] {
                if rng.chance(0.25 + 0.35 * density) {
                    snare.push((s, 0));
                }
            }
            vec![
                from_hits(Role::Kick, kick),
                from_hits(Role::Snare, &snare),
                hats(&EIGHTHS, &ODD, 1, 0.8, None, rng),
            ]
        }
        Genre::Dubstep => {
            let mut kick = vec![(0usize, 4u32)];
            if rng.chance(0.5) {
                kick.push((10, 4));
            }
            vec![
                from_hits(Role::Kick, &kick),
                from_hits(Role::Snare, &[(8, 4)]),
                hats(&EIGHTHS, &ODD, 1, 0.15 + 0.3 * density, None, rng),
            ]
        }
        Genre::Trap => {
            let kick: &[(usize, u32)] = [&[(0, 4), (7, 4), (10, 4)][..], &[(0, 4), (3, 4), (10, 4)][..], &[(0, 4), (10, 4), (13, 4)][..]][rng.below(3)];
            vec![
                from_hits(Role::Kick, kick),
                from_hits(Role::Snare, &[(8, 4)]),
                from_hits(Role::Clap, &[(8, 3)]),
                hats(&[(0, 3), (4, 3), (8, 3), (12, 3)], &SIXTEENTHS_OFF_KICK, 1, 0.9, None, rng),
                from_hits(Role::OpenHat, &if rng.chance(0.5) { vec![(14usize, 2u32)] } else { vec![] }),
            ]
        }
        Genre::UkGarage => {
            let kick: &[(usize, u32)] = if rng.chance(0.6) { &[(0, 4), (10, 4)] } else { &[(0, 4), (11, 4)] };
            vec![
                from_hits(Role::Kick, kick),
                from_hits(Role::Snare, &[(4, 4), (12, 4), (7, 0), (15, 0)]),
                hats(&[(2, 2), (6, 2), (10, 2), (14, 2)], &ODD, 1, weak, None, rng),
                from_hits(Role::OpenHat, &[(14, 2)]),
            ]
        }
        Genre::Reggaeton => vec![
            from_hits(Role::Kick, &four),
            from_hits(Role::Snare, &[(3, 3), (6, 3), (11, 3), (14, 3)]),
            hats(&EIGHTHS, &ODD, 1, 0.15 + 0.3 * density, None, rng),
        ],
        // House, pop and anything else.
        _ => vec![
            from_hits(Role::Kick, &four),
            from_hits(Role::Clap, &[(4, 3), (12, 3)]),
            hats(&[(2, 2), (10, 2)], &ODD, 1, weak, None, rng),
            from_hits(Role::OpenHat, &[(6, 2), (14, 2)]),
        ],
    }
}

/// The kick steps (one bar) a genre assumes when the project has no kick.
pub fn assumed_kick(genre: Genre) -> [bool; 16] {
    let mut k = [false; 16];
    let hits: &[usize] = match genre.style() {
        Genre::Dnb | Genre::UkGarage => &[0, 10],
        Genre::Dubstep => &[0],
        Genre::Trap => &[0, 7, 10],
        _ => &[0, 4, 8, 12],
    };
    for &h in hits {
        k[h] = true;
    }
    k
}

// ───────────────────────────── Melody & arp ─────────────────────────────

/// Rhythms for a lead line, from sparse to busy.
pub const MELODY_LANES: [&str; 8] = [
    "x.....x...x.....",
    "x..x....x..x....",
    "x..x..x...x.x...",
    "x.x...x.x...x.x.",
    "..x..x..x..x..x.",
    "x.xx..x.x.xx....",
    "x..x..x.x..x.x.x",
    "x.x.x.x.x.x.x.x.",
];

/// Accents for a step string by position in the beat: downbeats strong,
/// everything else normal (a lead line has no filler notes to play softly).
pub fn default_accents(steps: &str) -> String {
    steps
        .chars()
        .enumerate()
        .map(|(i, c)| match (c, i % 4) {
            ('.', _) => '.',
            (_, 0) => '3',
            _ => '2',
        })
        .collect()
}

/// The 3-3-2 accent steps of a bar of sixteenths.
pub const ACCENT_332: [usize; 6] = [0, 3, 6, 8, 11, 14];

/// For each global step of the clip, whether the kick falls on it: the
/// project's kick when there is one, else the genre's usual pattern.
pub fn kick_flags(ctx: &GenContext, genre: Genre, grid: &Grid) -> Vec<bool> {
    let assumed = assumed_kick(genre);
    (0..grid.total_steps())
        .map(|g| match &ctx.kick {
            Some(k) => k.at(g),
            None if grid.is_four_four() => assumed[(g % 16) as usize],
            None => (g % grid.steps_per_bar).is_multiple_of(4),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_layers(genre: Genre, seed: u64) -> Vec<Layer> {
        drum_layers(genre, 0.5, Energy::Mid, &mut Rng::new(seed))
    }

    #[test]
    fn every_lane_is_one_bar_with_matching_accents() {
        for g in Genre::ALL {
            for seed in 0..20 {
                for l in all_layers(g, seed) {
                    assert_eq!(l.steps.len(), 16, "{g:?} {:?}", l.role);
                    assert_eq!(l.accents.len(), 16);
                    for (s, a) in l.steps.chars().zip(l.accents.chars()) {
                        assert_eq!(s == '.', a == '.', "{g:?} {:?} {} {}", l.role, l.steps, l.accents);
                    }
                }
            }
        }
        for l in MELODY_LANES {
            assert_eq!(l.len(), 16);
        }
        for p in [Pattern::StabsOffbeat, Pattern::Sync332, Pattern::Charleston, Pattern::Pump8ths, Pattern::Whole] {
            let l = chord_lane(p);
            assert_eq!((l.steps.len(), l.accents.len()), (16, 16));
        }
    }

    #[test]
    fn house_kick_is_four_on_the_floor_with_a_backbeat_clap() {
        let kit = all_layers(Genre::House, 1);
        let kick = kit.iter().find(|l| l.role == Role::Kick).unwrap();
        assert_eq!(kick.steps, "x...x...x...x...");
        let clap = kit.iter().find(|l| l.role == Role::Clap).unwrap();
        assert_eq!(clap.steps, "....x.......x...");
    }

    #[test]
    fn every_genre_has_a_kick_and_a_backbeat_or_equivalent() {
        for g in Genre::ALL {
            let kit = all_layers(g, 3);
            assert!(kit.iter().any(|l| l.role == Role::Kick), "{g:?}");
            assert!(kit.len() >= 3, "{g:?}");
        }
    }

    #[test]
    fn rolling_bass_leaves_the_kick_steps_free() {
        let kick = assumed_kick(Genre::Techno);
        for seed in 0..50 {
            let (steps, acc) = bass_lane(Pattern::Rolling, &kick, 0.5, &mut Rng::new(seed));
            for s in [0usize, 4, 8, 12] {
                assert_eq!(steps.as_bytes()[s], b'.');
            }
            assert!(steps.chars().filter(|c| *c == 'x').count() >= 4);
            assert_eq!(acc.len(), 16);
        }
    }

    #[test]
    fn the_808_sits_on_the_kick() {
        let kick = assumed_kick(Genre::Trap);
        let (steps, _) = bass_lane(Pattern::Sub808, &kick, 0.5, &mut Rng::new(1));
        for (s, k) in kick.iter().enumerate() {
            if *k {
                assert_eq!(steps.as_bytes()[s], b'x');
            }
        }
    }

    #[test]
    fn default_accents_put_the_weight_on_the_beat() {
        assert_eq!(default_accents("x.x.xxx..x......"), "3.2.322..2......");
    }
}
