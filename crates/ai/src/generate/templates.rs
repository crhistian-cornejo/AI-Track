//! Templates: a request and the project context become a `MidiSpec` by
//! rules, with seeded choices. This is the whole generator when no model is
//! connected, and the fallback when a model's spec is unusable.
//!
//! The progressions are conventions per genre (docs/research/22 section
//! 2.1), written as degrees of the parent scale so they follow whatever key
//! the project is in; most are "diatonic" (the quality comes from stacking
//! thirds in the scale) and a few name an altered chord (the V9 with the
//! leading tone, the bII of phrygian).

use super::harmony::{Level, Parent, diatonic, to_spec};
use super::patterns::{self, MELODY_LANES};
use super::rng::Rng;
use super::spec::{ChordSpec, Contour, Gate, GenRequest, Genre, Kind, Layer, MidiSpec, Pattern, Quality, Role};
use super::tables;
use super::{GenContext, Grid, Params};

// ───────────────────────────── Progressions ─────────────────────────────

#[derive(Clone, Copy)]
enum Q {
    /// Stack thirds in the scale to this richness.
    D(Level),
    /// A named chord, possibly outside the scale.
    F(Quality),
}

/// (degree, alteration, chord)
type Step = (u8, i32, Q);

struct Prog {
    steps: &'static [Step],
    bars_per_chord: u32,
}

use Level::{Ninth as N9, Seventh as S7, Triad as T3};

/// Loops that start from the minor side (i = degree 0).
fn minor_progs(genre: Genre) -> &'static [Prog] {
    match genre.style() {
        Genre::DeepHouse => &[
            Prog { steps: &[(0, 0, Q::D(S7)), (3, 0, Q::D(S7)), (6, 0, Q::D(S7)), (2, 0, Q::D(S7))], bars_per_chord: 1 },
            Prog { steps: &[(0, 0, Q::D(N9)), (5, 0, Q::D(S7)), (6, 0, Q::D(N9)), (0, 0, Q::D(N9))], bars_per_chord: 1 },
            Prog { steps: &[(0, 0, Q::D(S7)), (3, 0, Q::F(Quality::Maj7)), (4, 0, Q::F(Quality::Dom9)), (0, 0, Q::D(S7))], bars_per_chord: 1 },
            Prog { steps: &[(0, 0, Q::D(N9)), (3, 0, Q::D(N9))], bars_per_chord: 2 },
            Prog { steps: &[(5, 0, Q::D(S7)), (3, 0, Q::D(S7)), (0, 0, Q::D(S7)), (4, 0, Q::D(S7))], bars_per_chord: 1 },
            Prog { steps: &[(0, 0, Q::D(S7)), (5, 0, Q::D(N9)), (2, 0, Q::D(S7)), (6, 0, Q::D(S7))], bars_per_chord: 1 },
        ],
        Genre::House | Genre::Pop => &[
            Prog { steps: &[(0, 0, Q::D(S7)), (5, 0, Q::D(S7)), (2, 0, Q::D(S7)), (6, 0, Q::D(S7))], bars_per_chord: 1 },
            Prog { steps: &[(0, 0, Q::D(S7)), (3, 0, Q::D(S7)), (6, 0, Q::D(S7)), (2, 0, Q::D(S7))], bars_per_chord: 1 },
            Prog { steps: &[(5, 0, Q::D(S7)), (3, 0, Q::D(S7)), (0, 0, Q::D(S7)), (6, 0, Q::D(S7))], bars_per_chord: 1 },
            Prog { steps: &[(0, 0, Q::D(S7)), (6, 0, Q::D(S7)), (5, 0, Q::D(S7)), (6, 0, Q::D(S7))], bars_per_chord: 1 },
            Prog { steps: &[(0, 0, Q::D(N9)), (5, 0, Q::D(S7)), (6, 0, Q::D(S7)), (0, 0, Q::D(S7))], bars_per_chord: 1 },
            Prog { steps: &[(0, 0, Q::D(S7)), (4, 0, Q::F(Quality::Dom7)), (0, 0, Q::D(S7)), (3, 0, Q::D(S7))], bars_per_chord: 1 },
        ],
        Genre::TechHouse => &[
            Prog { steps: &[(0, 0, Q::D(S7))], bars_per_chord: 4 },
            Prog { steps: &[(0, 0, Q::D(S7)), (6, 0, Q::D(S7))], bars_per_chord: 2 },
            Prog { steps: &[(0, 0, Q::D(S7)), (1, -1, Q::F(Quality::Maj7))], bars_per_chord: 2 },
            Prog { steps: &[(0, 0, Q::D(S7)), (5, 0, Q::D(S7))], bars_per_chord: 2 },
            Prog { steps: &[(0, 0, Q::D(S7)), (3, 0, Q::D(S7))], bars_per_chord: 2 },
            Prog { steps: &[(0, 0, Q::D(S7))], bars_per_chord: 2 },
        ],
        Genre::Techno => &[
            Prog { steps: &[(0, 0, Q::D(S7))], bars_per_chord: 4 },
            Prog { steps: &[(0, 0, Q::D(S7)), (6, 0, Q::D(S7))], bars_per_chord: 2 },
            Prog { steps: &[(0, 0, Q::D(S7)), (1, -1, Q::F(Quality::Maj7))], bars_per_chord: 2 },
            Prog { steps: &[(0, 0, Q::D(T3)), (5, 0, Q::D(T3))], bars_per_chord: 2 },
            Prog { steps: &[(0, 0, Q::D(S7)), (3, 0, Q::D(S7))], bars_per_chord: 2 },
            Prog { steps: &[(0, 0, Q::D(S7))], bars_per_chord: 2 },
        ],
        Genre::Trance => &[
            Prog { steps: &[(0, 0, Q::D(T3)), (5, 0, Q::D(T3)), (2, 0, Q::D(T3)), (6, 0, Q::D(T3))], bars_per_chord: 1 },
            Prog { steps: &[(0, 0, Q::D(T3)), (6, 0, Q::D(T3)), (5, 0, Q::D(T3)), (6, 0, Q::D(T3))], bars_per_chord: 1 },
            Prog { steps: &[(5, 0, Q::D(T3)), (6, 0, Q::D(T3)), (0, 0, Q::D(T3)), (0, 0, Q::D(T3))], bars_per_chord: 1 },
            Prog { steps: &[(0, 0, Q::D(T3)), (2, 0, Q::D(T3)), (6, 0, Q::D(T3)), (5, 0, Q::D(T3))], bars_per_chord: 1 },
            Prog { steps: &[(0, 0, Q::D(T3)), (3, 0, Q::D(T3)), (5, 0, Q::D(T3)), (6, 0, Q::D(T3))], bars_per_chord: 1 },
            Prog { steps: &[(0, 0, Q::D(S7)), (5, 0, Q::D(S7)), (2, 0, Q::D(S7)), (6, 0, Q::D(S7))], bars_per_chord: 1 },
        ],
        Genre::UkGarage => &[
            Prog { steps: &[(0, 0, Q::D(N9)), (3, 0, Q::D(N9)), (6, 0, Q::D(N9)), (2, 0, Q::D(N9))], bars_per_chord: 1 },
            Prog { steps: &[(1, 0, Q::D(N9)), (4, 0, Q::D(S7)), (0, 0, Q::D(N9)), (0, 0, Q::D(N9))], bars_per_chord: 1 },
            Prog { steps: &[(0, 0, Q::D(N9)), (5, 0, Q::D(N9)), (6, 0, Q::D(S7)), (0, 0, Q::D(N9))], bars_per_chord: 1 },
            Prog { steps: &[(5, 0, Q::D(N9)), (3, 0, Q::D(N9)), (0, 0, Q::D(N9)), (6, 0, Q::D(S7))], bars_per_chord: 1 },
        ],
        Genre::Dnb => &[
            Prog { steps: &[(0, 0, Q::D(S7)), (5, 0, Q::D(S7)), (2, 0, Q::D(S7)), (6, 0, Q::D(S7))], bars_per_chord: 1 },
            Prog { steps: &[(0, 0, Q::D(N9)), (3, 0, Q::D(N9))], bars_per_chord: 2 },
            Prog { steps: &[(0, 0, Q::D(S7)), (6, 0, Q::D(S7)), (5, 0, Q::D(S7)), (6, 0, Q::D(S7))], bars_per_chord: 1 },
            Prog { steps: &[(5, 0, Q::D(S7)), (2, 0, Q::D(S7)), (6, 0, Q::D(S7)), (0, 0, Q::D(S7))], bars_per_chord: 1 },
        ],
        Genre::Dubstep => &[
            Prog { steps: &[(0, 0, Q::D(T3)), (5, 0, Q::D(T3))], bars_per_chord: 2 },
            Prog { steps: &[(0, 0, Q::D(T3))], bars_per_chord: 4 },
            Prog { steps: &[(0, 0, Q::D(T3)), (6, 0, Q::D(T3)), (5, 0, Q::D(T3)), (5, 0, Q::D(T3))], bars_per_chord: 1 },
            Prog { steps: &[(0, 0, Q::D(T3)), (1, -1, Q::F(Quality::Maj))], bars_per_chord: 2 },
        ],
        Genre::Trap => &[
            Prog { steps: &[(0, 0, Q::D(N9)), (5, 0, Q::D(S7)), (6, 0, Q::D(S7)), (0, 0, Q::D(N9))], bars_per_chord: 1 },
            Prog { steps: &[(0, 0, Q::D(S7)), (3, 0, Q::D(S7)), (5, 0, Q::D(S7)), (6, 0, Q::D(S7))], bars_per_chord: 1 },
            Prog { steps: &[(0, 0, Q::D(N9)), (5, 0, Q::D(S7))], bars_per_chord: 2 },
            Prog { steps: &[(0, 0, Q::D(S7)), (6, 0, Q::D(S7)), (5, 0, Q::D(S7)), (4, 0, Q::D(S7))], bars_per_chord: 1 },
        ],
        Genre::Reggaeton => &[
            Prog { steps: &[(0, 0, Q::D(T3)), (3, 0, Q::D(T3)), (5, 0, Q::D(T3)), (4, 0, Q::F(Quality::Dom7))], bars_per_chord: 1 },
            Prog { steps: &[(0, 0, Q::D(T3)), (5, 0, Q::D(T3)), (2, 0, Q::D(T3)), (6, 0, Q::D(T3))], bars_per_chord: 1 },
            Prog { steps: &[(0, 0, Q::D(S7)), (3, 0, Q::D(S7)), (6, 0, Q::D(S7)), (2, 0, Q::D(S7))], bars_per_chord: 1 },
            Prog { steps: &[(5, 0, Q::D(T3)), (6, 0, Q::D(T3)), (0, 0, Q::D(T3)), (0, 0, Q::D(T3))], bars_per_chord: 1 },
        ],
        _ => &[Prog { steps: &[(0, 0, Q::D(S7)), (5, 0, Q::D(S7)), (2, 0, Q::D(S7)), (6, 0, Q::D(S7))], bars_per_chord: 1 }],
    }
}

/// Loops that start from the major side (I = degree 0).
fn major_progs(genre: Genre) -> &'static [Prog] {
    match genre.style() {
        Genre::DeepHouse => &[
            Prog { steps: &[(0, 0, Q::D(N9)), (5, 0, Q::D(N9)), (1, 0, Q::D(N9)), (4, 0, Q::D(N9))], bars_per_chord: 1 },
            Prog { steps: &[(1, 0, Q::D(N9)), (4, 0, Q::D(N9)), (0, 0, Q::D(N9)), (0, 0, Q::D(N9))], bars_per_chord: 1 },
            Prog { steps: &[(0, 0, Q::D(S7)), (3, 0, Q::D(S7)), (5, 0, Q::D(S7)), (4, 0, Q::D(S7))], bars_per_chord: 1 },
            Prog { steps: &[(0, 0, Q::D(N9)), (2, 0, Q::D(S7)), (3, 0, Q::D(N9)), (4, 0, Q::D(S7))], bars_per_chord: 1 },
        ],
        Genre::House | Genre::Pop | Genre::Reggaeton => &[
            Prog { steps: &[(0, 0, Q::D(S7)), (4, 0, Q::D(S7)), (5, 0, Q::D(S7)), (3, 0, Q::D(S7))], bars_per_chord: 1 },
            Prog { steps: &[(5, 0, Q::D(S7)), (3, 0, Q::D(S7)), (0, 0, Q::D(S7)), (4, 0, Q::D(S7))], bars_per_chord: 1 },
            Prog { steps: &[(0, 0, Q::D(T3)), (4, 0, Q::D(T3)), (5, 0, Q::D(T3)), (3, 0, Q::D(T3))], bars_per_chord: 1 },
            Prog { steps: &[(0, 0, Q::D(S7)), (5, 0, Q::D(S7)), (3, 0, Q::D(S7)), (4, 0, Q::D(S7))], bars_per_chord: 1 },
        ],
        Genre::TechHouse | Genre::Techno => &[
            Prog { steps: &[(0, 0, Q::D(S7))], bars_per_chord: 4 },
            Prog { steps: &[(0, 0, Q::D(S7)), (3, 0, Q::D(S7))], bars_per_chord: 2 },
            Prog { steps: &[(5, 0, Q::D(S7)), (3, 0, Q::D(S7))], bars_per_chord: 2 },
        ],
        Genre::Trance => &[
            Prog { steps: &[(0, 0, Q::D(T3)), (4, 0, Q::D(T3)), (5, 0, Q::D(T3)), (3, 0, Q::D(T3))], bars_per_chord: 1 },
            Prog { steps: &[(5, 0, Q::D(T3)), (3, 0, Q::D(T3)), (0, 0, Q::D(T3)), (4, 0, Q::D(T3))], bars_per_chord: 1 },
            Prog { steps: &[(0, 0, Q::D(T3)), (2, 0, Q::D(T3)), (3, 0, Q::D(T3)), (4, 0, Q::D(T3))], bars_per_chord: 1 },
        ],
        _ => &[
            Prog { steps: &[(0, 0, Q::D(S7)), (4, 0, Q::D(S7)), (5, 0, Q::D(S7)), (3, 0, Q::D(S7))], bars_per_chord: 1 },
            Prog { steps: &[(1, 0, Q::D(S7)), (4, 0, Q::D(S7)), (0, 0, Q::D(S7)), (0, 0, Q::D(S7))], bars_per_chord: 1 },
            Prog { steps: &[(5, 0, Q::D(S7)), (3, 0, Q::D(S7)), (0, 0, Q::D(S7)), (4, 0, Q::D(S7))], bars_per_chord: 1 },
        ],
    }
}

/// A progression filled to `bars` bars.
fn progression(parent: Parent, genre: Genre, bars: u32, beats_per_bar: f32, rng: &mut Rng) -> Vec<ChordSpec> {
    let major = parent.degree_iv(2) == 4;
    let pool = if major { major_progs(genre) } else { minor_progs(genre) };
    let prog = &pool[rng.below(pool.len())];
    let mut out = Vec::new();
    let mut bar = 0;
    let mut i = 0;
    while bar < bars {
        let (degree, alter, q) = prog.steps[i % prog.steps.len()];
        let (quality, ext) = match q {
            Q::D(level) => diatonic(parent, degree as i32, level),
            Q::F(q) => (q, Vec::new()),
        };
        let span = prog.bars_per_chord.min(bars - bar);
        out.push(ChordSpec { bar, beat: 0., beats: span as f32 * beats_per_bar, root: degree as u32, alter, quality, ext });
        bar += span;
        i += 1;
    }
    out
}

/// The chords a part follows: those of the other tracks when there are
/// some, else a progression of the genre.
fn harmony(req: &GenRequest, ctx: &GenContext, p: &Params, genre: Genre, bars: u32, rng: &mut Rng) -> Vec<ChordSpec> {
    let beats_per_bar = ctx.signature.numerator as f32 * 4. / ctx.signature.denominator.max(1) as f32;
    let parent = Parent::of(p.scale);
    if req.kind != Kind::Chords && ctx.has_harmony() {
        let mut out: Vec<ChordSpec> = Vec::new();
        for bar in 0..bars {
            let Some(sym) = ctx.chord_in_bar(bar) else { continue };
            let same = out.last().is_some_and(|c| {
                let last = super::harmony::resolve(c, parent);
                last.root == sym.root && last.quality == sym.quality
            });
            if same {
                if let Some(c) = out.last_mut() {
                    c.beats += beats_per_bar;
                }
            } else {
                out.push(to_spec(sym, parent, bar, 0., beats_per_bar));
            }
        }
        if !out.is_empty() {
            // A bar with no chord known keeps the one before it; the first starts the clip.
            out[0].bar = 0;
            let ends: Vec<u32> = out.iter().skip(1).map(|c| c.bar).chain(std::iter::once(bars)).collect();
            for (c, end) in out.iter_mut().zip(ends) {
                c.beats = (end - c.bar) as f32 * beats_per_bar;
            }
            return out;
        }
    }
    progression(parent, genre, bars, beats_per_bar, rng)
}

// ───────────────────────────── Layers ─────────────────────────────

/// A 16-step lane cut or repeated to the steps of a bar.
fn fit(lane: &str, steps_per_bar: usize) -> String {
    lane.chars().cycle().take(steps_per_bar).collect()
}

fn per_bar(grid: &Grid, mut lane: impl FnMut(u32) -> (String, String)) -> (String, String) {
    let (mut steps, mut accents) = (String::new(), String::new());
    for bar in 0..grid.bars {
        let (s, a) = lane(bar);
        steps.push_str(&fit(&s, grid.steps_per_bar as usize));
        accents.push_str(&fit(&a, grid.steps_per_bar as usize));
    }
    (steps, accents)
}

fn bass_layer(req: &GenRequest, ctx: &GenContext, p: &Params, genre: Genre, grid: &Grid, notes: &mut Vec<String>) -> Layer {
    let mut rng = Rng::part(p.seed, Role::Bass.id(), 0, 1);
    let pattern = match req.pattern {
        Some(x @ (Pattern::Rolling | Pattern::Offbeat | Pattern::Octaves | Pattern::RootFifth | Pattern::Whole | Pattern::Sub808 | Pattern::Sync332)) => x,
        _ => patterns::bass_pattern(genre, &mut rng),
    };
    let kick = patterns::kick_flags(ctx, genre, grid);
    let spb = grid.steps_per_bar as usize;
    let (steps, accents) = per_bar(grid, |bar| {
        let bar_kick = &kick[bar as usize * spb..(bar as usize + 1) * spb];
        let mut lane_rng = Rng::part(p.seed, Role::Bass.id(), bar as u64, 2);
        patterns::bass_lane(pattern, bar_kick, p.density, &mut lane_rng)
    });
    let (role, gate) = match pattern {
        Pattern::Sub808 => (Role::Sub, Gate::Long),
        Pattern::Whole | Pattern::Offbeat => (Role::Bass, Gate::Long),
        _ => (Role::Bass, Gate::Mid),
    };
    notes.push(
        match pattern {
            Pattern::Rolling => "Rolling bass: the first sixteenth of every beat is left for the kick",
            Pattern::Offbeat => "Offbeat bass between the kicks",
            Pattern::Octaves => "Bass jumping between root and octave on the offbeats",
            Pattern::RootFifth => "Root and fifth on the offbeats",
            Pattern::Whole => "One long note per chord",
            Pattern::Sub808 => "Sub bass locked to the kick, with a syncopation",
            _ => "Syncopated bass in a 3-3-2 shape",
        }
        .to_owned(),
    );
    Layer { role, pattern, steps, accents, pitches: patterns::bass_pitches(pattern), gate, ..Default::default() }
}

fn chord_layer(req: &GenRequest, p: &Params, genre: Genre, grid: &Grid, notes: &mut Vec<String>) -> Layer {
    let mut rng = Rng::part(p.seed, Role::Chord.id(), 0, 1);
    let pattern = match req.pattern {
        Some(x @ (Pattern::Whole | Pattern::StabsOffbeat | Pattern::Sync332 | Pattern::Charleston | Pattern::Pump8ths)) => x,
        _ => patterns::chord_pattern(genre, p.density, &mut rng),
    };
    let lane = patterns::chord_lane(pattern);
    let (steps, accents) = per_bar(grid, |_| (lane.steps.to_owned(), lane.accents.to_owned()));
    notes.push(
        match pattern {
            Pattern::StabsOffbeat => "Stabs on the offbeats, voiced to move as little as possible",
            Pattern::Sync332 => "Syncopated 3-3-2 stabs",
            Pattern::Charleston => "Charleston rhythm: long, then a short push",
            Pattern::Pump8ths => "Pumping eighths",
            _ => "Held chords with smooth voice leading",
        }
        .to_owned(),
    );
    Layer { role: Role::Chord, pattern, steps, accents, gate: tables::row(genre).chord_gate, ..Default::default() }
}

fn arp_layer(req: &GenRequest, p: &Params, grid: &Grid, notes: &mut Vec<String>) -> Layer {
    let mut rng = Rng::part(p.seed, Role::Arp.id(), 0, 1);
    let pattern = match req.pattern {
        Some(x @ (Pattern::Up | Pattern::Down | Pattern::UpDown | Pattern::Random)) => x,
        _ => [Pattern::Up, Pattern::UpDown, Pattern::UpDown, Pattern::Random, Pattern::Down][rng.below(5)],
    };
    let eighths = p.density < 0.35;
    let lane = if eighths { "x.x.x.x.x.x.x.x." } else { "xxxxxxxxxxxxxxxx" };
    let (steps, _) = per_bar(grid, |_| (lane.to_owned(), lane.to_owned()));
    notes.push(format!("Arpeggio {} in {}, accents grouped 3-3-2", format!("{pattern:?}").to_lowercase(), if eighths { "eighths" } else { "sixteenths" }));
    Layer { role: Role::Arp, pattern, steps, gate: Gate::Mid, ..Default::default() }
}

/// Moves in scale degrees for a contour.
fn motif_for(contour: Contour, n: usize, rng: &mut Rng) -> Vec<i32> {
    (0..n)
        .map(|i| {
            let half = n.div_ceil(2);
            let dir = match contour {
                Contour::Arch => if i < half { 1 } else { -1 },
                Contour::Rise => 1,
                Contour::Fall => -1,
                Contour::Wave => if (i / 2) % 2 == 0 { 1 } else { -1 },
                Contour::Flat => if rng.chance(0.5) { 1 } else { -1 },
            };
            // The first note is the anchor; after it, some notes repeat instead of moving.
            if i == 0 || rng.chance(if contour == Contour::Flat { 0.6 } else { 0.22 }) { 0 } else { dir }
        })
        .collect()
}

fn lead_layer(p: &Params, grid: &Grid, notes: &mut Vec<String>) -> Layer {
    let mut rng = Rng::part(p.seed, Role::Lead.id(), 0, 1);
    let base = (p.density * (MELODY_LANES.len() - 1) as f32).round() as i32 + rng.below(3) as i32 - 1;
    let lane = MELODY_LANES[base.clamp(0, MELODY_LANES.len() as i32 - 1) as usize];
    let contour = [Contour::Arch, Contour::Arch, Contour::Rise, Contour::Fall, Contour::Wave][rng.below(5)];
    let hits = lane.chars().filter(|c| *c == 'x').count().max(2);
    let motif = motif_for(contour, hits.min(super::spec::MAX_MOTIF), &mut rng);
    // Bar 3 (index 2) drops one hit so the repeat is not exact.
    let (steps, accents) = per_bar(grid, |bar| {
        let mut l: Vec<char> = lane.chars().collect();
        if bar % 4 == 2 {
            let ones: Vec<usize> = l.iter().enumerate().filter(|(i, c)| **c == 'x' && *i > 0).map(|(i, _)| i).collect();
            if ones.len() > 2 {
                let mut r = Rng::part(p.seed, Role::Lead.id(), bar as u64, 3);
                l[ones[r.below(ones.len())]] = '.';
            }
        }
        let s: String = l.into_iter().collect();
        (s.clone(), patterns::default_accents(&s))
    });
    notes.push(format!("{contour:?} contour, motif repeated and varied in the third bar"));
    Layer { role: Role::Lead, pattern: Pattern::Custom, steps, accents, contour, motif, gate: Gate::Mid, ..Default::default() }
}

fn drum_layers(req: &GenRequest, p: &Params, genre: Genre, grid: &Grid, notes: &mut Vec<String>) -> Vec<Layer> {
    // The first bar fixes the kick and the backbeat; the hats and ghosts alternate between two versions.
    let mut base = patterns::drum_layers(genre, p.density, req.energy, &mut Rng::part(p.seed, 0xD0, 0, 1));
    let alt = patterns::drum_layers(genre, p.density, req.energy, &mut Rng::part(p.seed, 0xD0, 1, 1));
    for l in &mut base {
        let varies = matches!(l.role, Role::Hat | Role::Perc | Role::Snare | Role::OpenHat);
        let other = alt.iter().find(|a| a.role == l.role);
        let (a, b) = (l.clone(), other.cloned().unwrap_or_else(|| l.clone()));
        let pick = |bar: u32| if varies && bar % 2 == 1 { &b } else { &a };
        let (steps, accents) = per_bar(grid, |bar| (pick(bar).steps.clone(), pick(bar).accents.clone()));
        l.steps = steps;
        l.accents = accents;
    }
    if req.kind == Kind::Perc {
        base.retain(|l| matches!(l.role, Role::Perc | Role::Hat | Role::OpenHat));
        if !base.iter().any(|l| l.role == Role::Perc) {
            let mut rng = Rng::part(p.seed, Role::Perc.id(), 0, 4);
            let (steps, accents) = per_bar(grid, |bar| {
                let mut r = Rng::part(p.seed, Role::Perc.id(), bar as u64, 5);
                let mut l = vec!['.'; 16];
                let mut a = vec!['.'; 16];
                for s in [3usize, 5, 7, 9, 11, 13, 15] {
                    if r.chance(0.2 + 0.4 * p.density) {
                        l[s] = 'x';
                        a[s] = if rng.chance(0.3) { '3' } else { '2' };
                    }
                }
                (l.into_iter().collect(), a.into_iter().collect())
            });
            base.push(Layer { role: Role::Perc, steps, accents, gate: Gate::Short, ..Default::default() });
        }
        notes.push("Percussion and hats only, to sit on top of an existing kick".to_owned());
    } else {
        notes.push(
            match genre {
                Genre::Techno => "Straight four on the floor, rim on 2 and 4, sixteenth hats accented on the offbeats",
                Genre::Dnb => "Two-step kick, snare on 2 and 4 with ghost notes, continuous hats",
                Genre::Trap | Genre::Dubstep => "Half-time: the snare lands on beat 3",
                Genre::UkGarage => "Two-step with the snare on 2 and 4 and swung hats",
                Genre::Reggaeton => "Dembow: four on the floor with the snare on the 3-3-2 points",
                _ => "Four on the floor, clap on 2 and 4, open hat on the offbeat",
            }
            .to_owned(),
        );
    }
    base
}

// ───────────────────────────── Entry ─────────────────────────────

/// The spec a template gives for `req` in `ctx`.
pub fn spec_for(req: &GenRequest, ctx: &GenContext, p: &Params) -> MidiSpec {
    let genre = req.genre.style();
    let bars = req.bars.clamp(1, super::spec::MAX_BARS);
    let grid = Grid::new(ctx.signature, bars);
    let row = tables::row(genre);
    let mut notes = Vec::new();
    let mut chord_rng = Rng::part(p.seed, 0xC0, 0, 0);
    let needs_chords = matches!(req.kind, Kind::Chords | Kind::Bass | Kind::Arp | Kind::Melody);
    let chords = if needs_chords { harmony(req, ctx, p, genre, bars, &mut chord_rng) } else { Vec::new() };
    let layers = match req.kind {
        Kind::Chords => vec![chord_layer(req, p, genre, &grid, &mut notes)],
        Kind::Bass => vec![bass_layer(req, ctx, p, genre, &grid, &mut notes)],
        Kind::Arp => vec![arp_layer(req, p, &grid, &mut notes)],
        Kind::Melody => vec![lead_layer(p, &grid, &mut notes)],
        Kind::Drums | Kind::Perc => drum_layers(req, p, genre, &grid, &mut notes),
    };
    if needs_chords && !chords.is_empty() {
        let parent = Parent::of(p.scale);
        let names: Vec<String> = chords.iter().take(4).map(|c| super::harmony::resolve(c, parent).name()).collect();
        let followed = if req.kind != Kind::Chords && ctx.has_harmony() { "following the chords of the project: " } else { "" };
        notes.push(format!("{followed}{}", names.join(" – ")));
    }
    MidiSpec { kind: req.kind, genre, bars, feel: req.feel.unwrap_or(row.feel), energy: req.energy, chords, layers, notes }.clamped()
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitrack_model::{Key, Scale, ScaleKind};

    fn req(kind: Kind, genre: Genre) -> GenRequest {
        GenRequest { genre, ..GenRequest::new(kind) }
    }

    fn params(scale: Scale, seed: u64) -> Params {
        Params { seed, humanize: true, scale, density: 0.5 }
    }

    #[test]
    fn every_genre_and_kind_gives_a_valid_spec() {
        let ctx = GenContext::default();
        for g in Genre::ALL {
            for k in Kind::ALL {
                for seed in 0..10 {
                    let s = spec_for(&req(k, g), &ctx, &params(Scale::new(Key(9), ScaleKind::Minor), seed));
                    assert!(s.has_content(), "{g:?} {k:?}");
                    assert_eq!(s, s.clone().clamped(), "templates stay inside the spec limits");
                    assert!(!s.notes.is_empty());
                }
            }
        }
    }

    #[test]
    fn deep_house_can_play_the_documented_loop() {
        // i7 – IVmaj7 – V9 – i7 appears for some seed.
        let ctx = GenContext::default();
        let found = (0..60).any(|seed| {
            let s = spec_for(&req(Kind::Chords, Genre::DeepHouse), &ctx, &params(Scale::new(Key(9), ScaleKind::Minor), seed));
            s.chords.iter().any(|c| c.quality == Quality::Dom9 && c.root == 4)
        });
        assert!(found);
    }

    #[test]
    fn progressions_fill_the_clip_and_follow_the_mode() {
        let ctx = GenContext::default();
        let major = spec_for(&GenRequest { bars: 8, ..req(Kind::Chords, Genre::House) }, &ctx, &params(Scale::new(Key(0), ScaleKind::Major), 3));
        let total: f32 = major.chords.iter().map(|c| c.beats).sum();
        assert_eq!(total, 32.);
        assert!(major.chords.iter().all(|c| matches!(c.quality, Quality::Maj7 | Quality::Min7 | Quality::Dom7 | Quality::Maj9 | Quality::Min9 | Quality::Dom9 | Quality::Maj | Quality::Min)));
        assert_eq!(major.chords[0].bar, 0);
    }

    #[test]
    fn a_bass_follows_the_chords_already_in_the_project() {
        use super::super::harmony::ChordSym;
        let ctx = GenContext { harmony: vec![Some(ChordSym::new(9, Quality::Min7, vec![])), Some(ChordSym::new(9, Quality::Min7, vec![])), Some(ChordSym::new(5, Quality::Maj7, vec![])), Some(ChordSym::new(7, Quality::Dom7, vec![]))], ..Default::default() };
        let s = spec_for(&req(Kind::Bass, Genre::House), &ctx, &params(Scale::new(Key(9), ScaleKind::Minor), 1));
        assert_eq!(s.chords.len(), 3);
        assert_eq!((s.chords[0].root, s.chords[0].beats), (0, 8.));
        assert_eq!((s.chords[1].root, s.chords[2].root), (5, 6));
    }

    #[test]
    fn rolling_bass_is_one_string_per_bar() {
        let ctx = GenContext::default();
        let s = spec_for(&GenRequest { bars: 2, pattern: Some(Pattern::Rolling), ..req(Kind::Bass, Genre::Techno) }, &ctx, &params(Scale::new(Key(9), ScaleKind::Minor), 5));
        assert_eq!(s.layers[0].steps.len(), 32);
        assert_eq!(s.layers[0].role, Role::Bass);
        assert!(s.layers[0].steps.chars().skip(16).step_by(4).all(|c| c == '.'), "kick steps stay free");
    }

    #[test]
    fn trap_bass_is_a_sub_locked_to_the_kick() {
        let s = spec_for(&req(Kind::Bass, Genre::Trap), &GenContext::default(), &params(Scale::new(Key(9), ScaleKind::Minor), 5));
        assert_eq!((s.layers[0].role, s.layers[0].pattern), (Role::Sub, Pattern::Sub808));
    }

    #[test]
    fn hats_alone_keep_only_the_hat_layers() {
        let r = GenRequest { only: vec![Role::Hat, Role::OpenHat], ..req(Kind::Drums, Genre::House) };
        let g = super::super::generate(&r, &GenContext::default(), 1);
        assert!(g.spec.layers.iter().all(|l| matches!(l.role, Role::Hat | Role::OpenHat)));
    }
}
