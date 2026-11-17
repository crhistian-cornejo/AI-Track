//! The quality tests of docs/research/22-generador-midi.md section 7.1:
//! properties every result must have for any seed, genre, key and scale.
//! They check what must survive any tuning of `tables.rs`/`patterns.rs`,
//! not the exact notes.

use aitrack_model::{Key, Scale, ScaleKind, Tick, TimeSignature};

use super::context::{KickMap, PadMap};
use super::feel::{Timing, swing_delay};
use super::harmony::{Parent, parallel};
use super::spec::{Energy, Feel, GenRequest, Genre, Kind, Role};
use super::tables::{self, KickPolicy, MONO_GAP};
use super::{GenContext, Generated, generate, patterns};

const SCALES: [ScaleKind; 10] = [
    ScaleKind::Major,
    ScaleKind::Minor,
    ScaleKind::HarmonicMinor,
    ScaleKind::MelodicMinor,
    ScaleKind::Dorian,
    ScaleKind::Phrygian,
    ScaleKind::Lydian,
    ScaleKind::Mixolydian,
    ScaleKind::Locrian,
    ScaleKind::MinorPentatonic,
];

fn ctx_for(genre: Genre, sig: TimeSignature, bpm: f64) -> GenContext {
    let mut c = GenContext { signature: sig, bpm, ..Default::default() };
    if sig == TimeSignature::default() {
        c.kick = Some(KickMap { steps: patterns::assumed_kick(genre).to_vec() });
    }
    c
}

fn request(kind: Kind, genre: Genre, bars: u32, scale: Scale) -> GenRequest {
    GenRequest { genre, bars, key: Some(scale), ..GenRequest::new(kind) }
}

/// One generation for the sweep position `i` of a (seed, genre, kind) loop.
fn one(seed: u64, genre: Genre, kind: Kind) -> (Generated, GenContext) {
    let tonic = ((seed * 7 + genre as u64 * 5) % 12) as u8;
    let kind_of_scale = SCALES[((seed / 3 + genre as u64) % SCALES.len() as u64) as usize];
    let bars = [1, 2, 4, 8][(seed % 4) as usize];
    let ctx = ctx_for(genre, TimeSignature::default(), 124.);
    let g = generate(&request(kind, genre, bars, Scale::new(Key(tonic), kind_of_scale)), &ctx, seed);
    (g, ctx)
}

/// Every (seed, genre, kind) of the sweep.
fn sweep(mut f: impl FnMut(u64, Genre, Kind, &Generated, &GenContext)) {
    for seed in 0..200u64 {
        for genre in Genre::ALL {
            for kind in Kind::ALL {
                let (g, ctx) = one(seed, genre, kind);
                f(seed, genre, kind, &g, &ctx);
            }
        }
    }
}

/// Every tonic and scale once, for each genre and kind.
fn key_sweep(mut f: impl FnMut(Scale, Genre, Kind, &Generated)) {
    for tonic in 0..12u8 {
        for sk in SCALES {
            for genre in Genre::ALL {
                for kind in Kind::ALL {
                    let scale = Scale::new(Key(tonic), sk);
                    let ctx = ctx_for(genre, TimeSignature::default(), 124.);
                    let g = generate(&request(kind, genre, 4, scale), &ctx, tonic as u64 * 31 + sk as u64);
                    f(scale, genre, kind, &g);
                }
            }
        }
    }
}

fn who(g: &Generated, kind: Kind, seed: u64) -> String {
    format!("{kind:?} {} seed {seed}: {:?}", g.interpretation, g.report.fixes)
}

// ───────────────────────────── Basics ─────────────────────────────

#[test]
fn every_kind_and_genre_gives_notes() {
    sweep(|seed, _, kind, g, _| {
        assert!(!g.draft.notes.is_empty(), "nothing for {}", who(g, kind, seed));
    });
}

#[test]
fn notes_in_scale_or_declared() {
    let check = |scale: Scale, g: &Generated, label: &str| {
        let parent = Parent::of(scale);
        for role in [Role::Bass, Role::Sub, Role::Chord, Role::Arp, Role::Lead] {
            let notes: Vec<_> = g.draft.of_role(role).collect();
            let mut passing = 0;
            for n in &notes {
                if !parent.contains_pc(n.key % 12) {
                    assert_ne!(n.declared, super::draft::Declared::No, "{label}: {role:?} note {} is not in {} and not declared", n.key, scale.display());
                }
                passing += (n.declared == super::draft::Declared::Passing) as usize;
            }
            let cap = (notes.len() as f32 * tables::MAX_PASSING).floor() as usize;
            assert!(passing <= cap, "{label}: {role:?} declares {passing} of {}", notes.len());
        }
    };
    sweep(|seed, _, kind, g, _| check(g.params.scale, g, &who(g, kind, seed)));
    key_sweep(|scale, _, kind, g| check(scale, g, &format!("{kind:?} {}", scale.display())));
}

#[test]
fn inside_the_clip() {
    sweep(|seed, _, kind, g, _| {
        for n in &g.draft.notes {
            assert!(n.start >= 0 && n.start < g.clip_ticks && n.end() <= g.clip_ticks && n.length > 0, "{}: note {n:?} vs clip {}", who(g, kind, seed), g.clip_ticks);
        }
    });
}

#[test]
fn within_register_by_role() {
    sweep(|seed, _, kind, g, ctx| {
        let pads = ctx.target.pads.keys();
        for n in &g.draft.notes {
            if n.role.is_drum() {
                assert!(pads.contains(&n.key), "{}: drum key {} not in the kit", who(g, kind, seed), n.key);
            } else {
                let l = tables::limits(n.role);
                assert!((l.lo..=l.hi).contains(&n.key), "{}: {:?} note {} outside {}..={}", who(g, kind, seed), n.role, n.key, l.lo, l.hi);
            }
        }
    });
}

#[test]
fn mono_roles_do_not_overlap() {
    sweep(|seed, _, kind, g, _| {
        for role in [Role::Bass, Role::Sub, Role::Lead, Role::Arp] {
            let mut v: Vec<_> = g.draft.of_role(role).collect();
            v.sort_by_key(|n| n.start);
            for w in v.windows(2) {
                assert!(w[0].end() <= w[1].start - MONO_GAP, "{}: {role:?} {:?} runs into {:?}", who(g, kind, seed), w[0], w[1]);
            }
        }
    });
}

#[test]
fn polyphony_cap() {
    sweep(|seed, _, kind, g, _| {
        for role in [Role::Bass, Role::Sub, Role::Chord, Role::Lead, Role::Arp] {
            let v: Vec<_> = g.draft.of_role(role).collect();
            for n in &v {
                let sounding = v.iter().filter(|m| m.start <= n.start && m.end() > n.start).count();
                assert!(sounding <= tables::limits(role).poly, "{}: {role:?} has {sounding} at once", who(g, kind, seed));
            }
        }
    });
}

#[test]
fn min_lengths() {
    sweep(|seed, _, kind, g, _| {
        for n in &g.draft.notes {
            assert!(n.length >= tables::limits(n.role).min_len.min(g.clip_ticks - n.start), "{}: {n:?}", who(g, kind, seed));
            assert!((0.02..=1.0).contains(&n.velocity));
        }
    });
}

// ───────────────────────────── The rules that are heard most ─────────────────────────────

#[test]
fn kick_bass_clash() {
    let mut avoid_checked = 0;
    let mut lock_checked = 0;
    sweep(|seed, genre, kind, g, _| {
        if kind != Kind::Bass {
            return;
        }
        let kick = patterns::assumed_kick(genre);
        let on_kick = |nominal: Tick| kick[((nominal / 240) % 16) as usize];
        let bass: Vec<_> = g.draft.notes.iter().filter(|n| matches!(n.role, Role::Bass | Role::Sub)).collect();
        match tables::row(genre).kick_policy {
            KickPolicy::Avoid => {
                avoid_checked += bass.len();
                let clashes = bass.iter().filter(|n| on_kick(n.nominal)).count();
                assert_eq!(clashes, 0, "{}: bass starts on the kick", who(g, kind, seed));
            }
            KickPolicy::Lock => {
                lock_checked += bass.len();
                if bass.len() >= 5 {
                    let together = bass.iter().filter(|n| on_kick(n.nominal)).count();
                    assert!(together as f32 >= 0.8 * bass.len() as f32, "{}: only {together} of {} on the kick", who(g, kind, seed), bass.len());
                }
            }
            KickPolicy::Free => {}
        }
    });
    assert!(avoid_checked > 1000 && lock_checked > 100, "{avoid_checked} {lock_checked}");
}

#[test]
fn the_bass_follows_the_projects_kick_not_an_assumed_one() {
    // A kick on the offbeats: the bass must leave those steps, not 0/4/8/12.
    let mut ctx = ctx_for(Genre::Techno, TimeSignature::default(), 130.);
    let offbeat: Vec<bool> = (0..16).map(|s| s % 4 == 2).collect();
    ctx.kick = Some(KickMap { steps: offbeat });
    for seed in 0..30 {
        let g = generate(&request(Kind::Bass, Genre::Techno, 2, Scale::new(Key(9), ScaleKind::Minor)), &ctx, seed);
        assert!(g.draft.notes.iter().all(|n| (n.nominal / 240) % 4 != 2), "seed {seed}");
        assert!(g.draft.notes.len() >= 8);
    }
}

#[test]
fn velocity_not_flat() {
    let mut ghosts = std::collections::HashMap::<Genre, usize>::new();
    sweep(|seed, genre, kind, g, _| {
        if kind != Kind::Drums {
            return;
        }
        let hats: Vec<f32> = g.draft.of_role(Role::Hat).map(|n| n.velocity).collect();
        if hats.len() >= 4 {
            let mean = hats.iter().sum::<f32>() / hats.len() as f32;
            let sd = (hats.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / hats.len() as f32).sqrt();
            assert!(sd >= 0.08, "{}: hats sd {sd:.3}", who(g, kind, seed));
            for bar in 0..g.bars {
                let mut distinct: Vec<u32> = g.draft.of_role(Role::Hat).filter(|n| n.bar(&g.draft.grid) == bar).map(|n| (n.velocity * 127.).round() as u32).collect();
                distinct.sort_unstable();
                distinct.dedup();
                assert!(distinct.len() >= 4, "{}: bar {bar} has {} distinct hat velocities", who(g, kind, seed), distinct.len());
            }
        }
        let kicks: Vec<f32> = g.draft.of_role(Role::Kick).filter(|n| n.velocity > 0.7).map(|n| n.velocity).collect();
        if kicks.len() >= 3 {
            let mean = kicks.iter().sum::<f32>() / kicks.len() as f32;
            let sd = (kicks.iter().map(|v| (v - mean).powi(2)).sum::<f32>() / kicks.len() as f32).sqrt();
            assert!(sd <= 0.04, "{}: kick sd {sd:.3}", who(g, kind, seed));
        }
        for n in g.draft.of_role(Role::Snare) {
            assert!(n.velocity < 0.3 || n.velocity > 0.5, "{}: a snare of {:.2} is neither ghost nor hit", who(g, kind, seed), n.velocity);
            *ghosts.entry(genre).or_default() += (n.velocity < 0.3) as usize;
        }
    });
    for g in Genre::ALL {
        let uses = tables::row(g).ghosts && matches!(g, Genre::Dnb | Genre::UkGarage);
        assert_eq!(ghosts.get(&g).copied().unwrap_or(0) > 0, uses, "ghost notes in {g:?}");
    }
}

#[test]
fn accent_pattern_by_genre() {
    // House: the offbeat hats are louder than the filler sixteenths.
    let mut main = Vec::new();
    let mut filler = Vec::new();
    for seed in 0..100 {
        let ctx = ctx_for(Genre::House, TimeSignature::default(), 124.);
        let g = generate(&request(Kind::Drums, Genre::House, 4, Scale::new(Key(9), ScaleKind::Minor)), &ctx, seed);
        for n in g.draft.of_role(Role::Hat) {
            let step = (n.nominal / 240) % 16;
            if step == 2 || step == 10 {
                main.push(n.velocity);
            } else if step % 2 == 1 {
                filler.push(n.velocity);
            }
        }
    }
    let mean = |v: &[f32]| v.iter().sum::<f32>() / v.len() as f32;
    assert!(main.len() > 100 && filler.len() > 100);
    assert!(mean(&main) > mean(&filler) + 0.1, "{} vs {}", mean(&main), mean(&filler));
}

#[test]
fn seeds_differ() {
    for genre in Genre::ALL {
        for kind in Kind::ALL {
            let mut same_grid = 0;
            for i in 0..20u64 {
                let scale = Scale::new(Key(9), ScaleKind::Minor);
                let ctx = ctx_for(genre, TimeSignature::default(), 124.);
                let a = generate(&request(kind, genre, 4, scale), &ctx, i);
                let b = generate(&request(kind, genre, 4, scale), &ctx, i + 1000);
                let grid = |g: &Generated| g.draft.notes.iter().map(|n| (n.key, n.nominal)).collect::<Vec<_>>();
                assert_ne!(format!("{:?}", a.draft.notes), format!("{:?}", b.draft.notes), "{genre:?} {kind:?} seeds {i} and {}", i + 1000);
                same_grid += (grid(&a) == grid(&b)) as usize;
            }
            assert!(same_grid <= 4, "{genre:?} {kind:?}: {same_grid} of 20 seed pairs give the same pattern");
        }
    }
}

#[test]
fn same_seed_same_notes() {
    sweep(|seed, genre, kind, g, ctx| {
        let scale = g.params.scale;
        let again = generate(&request(kind, genre, g.bars, scale), ctx, seed);
        assert_eq!(format!("{:?}", g.draft.notes), format!("{:?}", again.draft.notes), "{}", who(g, kind, seed));
        assert_eq!(g.spec, again.spec);
    });
}

#[test]
fn swing_matches_formula() {
    let scale = Scale::new(Key(9), ScaleKind::Minor);
    for seed in 0..40 {
        for (kind, genre) in [(Kind::Drums, Genre::House), (Kind::Drums, Genre::TechHouse), (Kind::Arp, Genre::House), (Kind::Drums, Genre::UkGarage)] {
            let ctx = ctx_for(genre, TimeSignature::default(), 124.);
            let req = GenRequest { feel: Some(Feel::Swing), humanize: false, ..request(kind, genre, 4, scale) };
            let g = generate(&req, &ctx, seed);
            let delay = swing_delay(tables::swing(genre, Feel::Swing));
            assert!(delay > 0);
            let mut swung = 0;
            for n in &g.draft.notes {
                let odd = (n.nominal / 240) % 2 == 1;
                let expect = if odd && tables::swung(Feel::Swing, n.role) { delay } else { 0 };
                assert_eq!(n.start - n.nominal, expect, "{kind:?} {genre:?} {:?} step {}", n.role, n.nominal / 240);
                swung += (expect > 0) as usize;
            }
            assert!(swung > 0, "{kind:?} {genre:?} had nothing to swing");
        }
    }
}

#[test]
fn microtiming_bounded() {
    let scale = Scale::new(Key(9), ScaleKind::Minor);
    for bpm in [80., 124., 174.] {
        for genre in Genre::ALL {
            for kind in Kind::ALL {
                for seed in 0..10 {
                    let ctx = ctx_for(genre, TimeSignature::default(), bpm);
                    let g = generate(&GenRequest { feel: Some(Feel::Straight), ..request(kind, genre, 4, scale) }, &ctx, seed);
                    let t = Timing { genre, feel: Feel::Straight, bpm, humanize: true };
                    for n in &g.draft.notes {
                        let strum = if n.role == Role::Chord { super::grid::ms_to_ticks(tables::strum_bound_ms(genre), bpm) } else { 0 };
                        let bound = t.micro_bound(n.role) + strum;
                        assert!((n.start - n.nominal).abs() <= bound, "{kind:?} {genre:?} {:?} at {bpm} BPM shifted {} > {bound}", n.role, n.start - n.nominal);
                    }
                }
            }
        }
    }
}

// ───────────────────────────── Harmony ─────────────────────────────

/// The voicings of a chords result, one per change, as sorted keys.
fn voicings(g: &Generated) -> Vec<Vec<u8>> {
    let mut by_onset: std::collections::BTreeMap<Tick, Vec<u8>> = Default::default();
    for n in g.draft.of_role(Role::Chord) {
        by_onset.entry(n.nominal).or_default().push(n.key);
    }
    let mut out: Vec<Vec<u8>> = Vec::new();
    for (_, mut keys) in by_onset {
        keys.sort_unstable();
        if out.last() != Some(&keys) {
            out.push(keys);
        }
    }
    out
}

#[test]
fn voice_leading_smooth() {
    let (mut transitions, mut smooth) = (0, 0);
    sweep(|seed, _, kind, g, _| {
        if kind != Kind::Chords {
            return;
        }
        for w in voicings(g).windows(2) {
            let jump = (*w[1].last().unwrap() as i32 - *w[0].last().unwrap() as i32).abs();
            assert!(jump <= tables::TOP_VOICE_JUMP_MAX, "{}: top voice jumps {jump}: {:?} -> {:?}", who(g, kind, seed), w[0], w[1]);
            transitions += 1;
            smooth += (jump <= tables::TOP_VOICE_JUMP_WARN) as usize;
            if w[0].len() == w[1].len() && w[0].len() > 1 {
                let (a, b) = (&w[0], &w[1]);
                assert!(!parallel(a[0], a[a.len() - 1], b[0], b[b.len() - 1]), "{}: parallel fifths or octaves between bass and top: {a:?} -> {b:?}", who(g, kind, seed));
            }
        }
    });
    assert!(transitions > 1000, "{transitions}");
    assert!(smooth as f32 >= 0.9 * transitions as f32, "{smooth} of {transitions} transitions move the top voice by 5 or less");
}

#[test]
fn common_tones_kept() {
    let (mut shared, mut kept) = (0, 0);
    sweep(|_, _, kind, g, _| {
        if kind != Kind::Chords {
            return;
        }
        for w in voicings(g).windows(2) {
            let pcs = |v: &[u8]| v.iter().map(|k| k % 12).collect::<std::collections::BTreeSet<_>>();
            if pcs(&w[0]).intersection(&pcs(&w[1])).next().is_some() {
                shared += 1;
                kept += w[0].iter().any(|k| w[1].contains(k)) as usize;
            }
        }
    });
    assert!(shared > 500);
    assert!(kept as f32 >= 0.9 * shared as f32, "{kept} of {shared}");
}

#[test]
fn chords_carry_sevenths_in_house_and_guide_tones_stay() {
    for seed in 0..60 {
        for genre in [Genre::House, Genre::DeepHouse, Genre::TechHouse] {
            let ctx = ctx_for(genre, TimeSignature::default(), 124.);
            let g = generate(&request(Kind::Chords, genre, 4, Scale::new(Key(9), ScaleKind::Minor)), &ctx, seed);
            let sevenths = g.spec.chords.iter().filter(|c| !matches!(c.quality, super::spec::Quality::Maj | super::spec::Quality::Min | super::spec::Quality::Dim | super::spec::Quality::Aug | super::spec::Quality::Sus2 | super::spec::Quality::Sus4)).count();
            assert!(sevenths * 4 >= g.spec.chords.len() * 3, "{genre:?} seed {seed}: {:?}", g.spec.chords);
        }
    }
}

#[test]
fn arp_not_identical_every_bar() {
    for seed in 0..100 {
        for genre in Genre::ALL {
            let ctx = ctx_for(genre, TimeSignature::default(), 124.);
            let g = generate(&request(Kind::Arp, genre, 4, Scale::new(Key(2), ScaleKind::Dorian)), &ctx, seed);
            let bars: Vec<Vec<(u8, Tick)>> = (0..4).map(|b| g.draft.notes.iter().filter(|n| n.bar(&g.draft.grid) == b).map(|n| (n.key, n.nominal - b as Tick * g.draft.grid.bar_ticks)).collect()).collect();
            let mut distinct = bars.clone();
            distinct.sort();
            distinct.dedup();
            assert!(distinct.len() >= 2, "{genre:?} seed {seed}: every bar of the arp is the same");
        }
    }
}

#[test]
fn melody_stepwise() {
    let (mut intervals, mut stepwise) = (0, 0);
    sweep(|seed, _, kind, g, _| {
        if kind != Kind::Melody {
            return;
        }
        let mut notes: Vec<_> = g.draft.of_role(Role::Lead).collect();
        notes.sort_by_key(|n| n.start);
        let keys: Vec<i32> = notes.iter().map(|n| n.key as i32).collect();
        let range = keys.iter().max().unwrap() - keys.iter().min().unwrap();
        assert!(range <= tables::MELODY_MAX_RANGE, "{}: range {range}", who(g, kind, seed));
        if keys.len() >= 6 {
            let ok = keys.windows(2).filter(|w| (w[1] - w[0]).abs() <= 2).count();
            assert!(ok as f32 >= tables::MELODY_STEPWISE_MIN * (keys.len() - 1) as f32, "{}: {ok} of {} steps are small: {keys:?}", who(g, kind, seed), keys.len() - 1);
            intervals += keys.len() - 1;
            stepwise += ok;
        }
    });
    assert!(intervals > 2000, "{intervals} {stepwise}");
}

// ───────────────────────────── Robustness ─────────────────────────────

#[test]
fn empty_project_gives_defaults() {
    for kind in Kind::ALL {
        let g = generate(&GenRequest::new(kind), &GenContext::default(), 0);
        assert!(!g.draft.notes.is_empty(), "{kind:?}");
        assert_eq!(g.params.scale, Scale::new(Key(9), ScaleKind::Minor));
        assert!(!g.interpretation.is_empty());
    }
}

#[test]
fn tempo_variants() {
    let scale = Scale::new(Key(5), ScaleKind::Mixolydian);
    let sigs = [TimeSignature::default(), TimeSignature { numerator: 3, denominator: 4 }, TimeSignature { numerator: 6, denominator: 8 }, TimeSignature { numerator: 7, denominator: 8 }, TimeSignature { numerator: 5, denominator: 4 }];
    for bpm in [60., 90., 124., 140., 174.] {
        for sig in sigs {
            for kind in Kind::ALL {
                for genre in [Genre::House, Genre::Techno, Genre::Dnb, Genre::Trap] {
                    for seed in 0..4 {
                        let ctx = ctx_for(genre, sig, bpm);
                        let g = generate(&request(kind, genre, 3, scale), &ctx, seed);
                        assert_eq!(g.clip_ticks, sig.bar_ticks() * 3);
                        assert!(!g.draft.notes.is_empty(), "{kind:?} {genre:?} {sig:?}");
                        for n in &g.draft.notes {
                            assert!(n.start >= 0 && n.start < g.clip_ticks && n.end() <= g.clip_ticks, "{kind:?} {genre:?} {sig:?} {bpm}: {n:?}");
                        }
                    }
                }
            }
        }
    }
}

#[test]
fn drum_pad_mapping() {
    let pads = PadMap::from_pads(&[("BD".to_owned(), 36), ("SD".to_owned(), 38), ("CH".to_owned(), 42)]);
    let mut ctx = ctx_for(Genre::House, TimeSignature::default(), 124.);
    ctx.target.pads = pads;
    let g = generate(&request(Kind::Drums, Genre::House, 4, Scale::new(Key(9), ScaleKind::Minor)), &ctx, 1);
    assert!(g.draft.notes.iter().all(|n| [36, 38, 42].contains(&n.key)), "{:?}", g.draft.notes.iter().map(|n| n.key).collect::<std::collections::BTreeSet<_>>());
    assert!(g.draft.of_role(Role::Kick).count() > 0 && g.draft.of_role(Role::Hat).count() > 0);
    assert!(g.report.warnings.iter().any(|w| w.contains("open hat")), "{:?}", g.report.warnings);
    // The clap falls back to the snare pad when the kit has no clap, so the backbeat survives.
    assert!(g.draft.of_role(Role::Clap).all(|n| n.key == 38) && g.draft.of_role(Role::Clap).count() > 0);
    assert_eq!(g.draft.of_role(Role::OpenHat).count(), 0);
}

#[test]
fn a_model_spec_goes_through_the_same_checks() {
    use super::spec::{ChordSpec, Layer, MidiSpec, Pattern, Quality};
    // A sloppy spec: the bass wants to start on the kick, chords are extreme, steps are junk.
    let spec = MidiSpec {
        kind: Kind::Bass,
        genre: Genre::Techno,
        bars: 2,
        chords: vec![ChordSpec { root: 9, alter: 5, quality: Quality::Dom9, ext: vec!["13".into(), "zz".into()], ..Default::default() }],
        layers: vec![Layer { role: Role::Bass, pattern: Pattern::Custom, steps: "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx".into(), octave: 9, ..Default::default() }],
        ..Default::default()
    };
    let ctx = ctx_for(Genre::Techno, TimeSignature::default(), 130.);
    let g = super::generate_from_spec(&request(Kind::Bass, Genre::Techno, 2, Scale::new(Key(0), ScaleKind::Minor)), &ctx, spec, 3);
    assert!(!g.draft.notes.is_empty());
    for n in &g.draft.notes {
        assert!((28..=55).contains(&n.key) && n.end() <= g.clip_ticks);
        assert!(!patterns::assumed_kick(Genre::Techno)[((n.nominal / 240) % 16) as usize]);
    }
    // And an empty spec falls back to a template.
    let g = super::generate_from_spec(&request(Kind::Bass, Genre::Techno, 2, Scale::new(Key(0), ScaleKind::Minor)), &ctx, MidiSpec::default(), 3);
    assert_eq!(g.source, super::Source::Template);
    assert!(!g.draft.notes.is_empty());
    let _ = Energy::Mid;
}

#[test]
fn a_part_keeps_its_dice_when_the_others_are_left_out() {
    // Asking for the hats alone gives the very hats the full kit has for the same seed.
    let scale = Scale::new(Key(9), ScaleKind::Minor);
    for seed in 0..40 {
        let ctx = ctx_for(Genre::House, TimeSignature::default(), 124.);
        let full = generate(&request(Kind::Drums, Genre::House, 4, scale), &ctx, seed);
        let alone = generate(&GenRequest { only: vec![Role::Hat, Role::OpenHat], ..request(Kind::Drums, Genre::House, 4, scale) }, &ctx, seed);
        let pick = |g: &Generated, role| g.draft.of_role(role).map(|n| (n.key, n.start, n.length, (n.velocity * 1000.) as i32)).collect::<Vec<_>>();
        assert_eq!(pick(&full, Role::Hat), pick(&alone, Role::Hat), "seed {seed}");
        assert_eq!(pick(&full, Role::OpenHat), pick(&alone, Role::OpenHat), "seed {seed}");
        assert!(alone.draft.of_role(Role::Kick).count() == 0 && !pick(&alone, Role::Hat).is_empty());
    }
}

#[test]
fn a_model_spec_that_needs_many_repairs_is_replaced_and_says_so() {
    use super::spec::{Layer, MidiSpec, Pattern};
    // A bass that lands on every sixteenth, kick steps included, in a techno bar.
    let spec = MidiSpec {
        kind: Kind::Bass,
        genre: Genre::Techno,
        bars: 2,
        layers: vec![Layer { role: Role::Bass, pattern: Pattern::Custom, steps: "x".repeat(32), accents: "3".repeat(32), ..Default::default() }],
        ..Default::default()
    };
    let ctx = ctx_for(Genre::Techno, TimeSignature::default(), 130.);
    let r = request(Kind::Bass, Genre::Techno, 2, Scale::new(Key(9), ScaleKind::Minor));
    let g = super::generate_from_spec(&r, &ctx, spec, 5);
    assert_eq!(g.source, super::Source::Template, "{:?}", g.report.fixes);
    let why = g.fallback.as_deref().unwrap_or_default();
    assert!(why.contains("repairs") || why.contains("no notes"), "{why}");
    assert!(!g.draft.notes.is_empty());
    // A bass layer with no chords still plays, on the tonic chord.
    let no_chords = MidiSpec { chords: vec![], layers: vec![Layer { role: Role::Bass, pattern: Pattern::Offbeat, ..Default::default() }], kind: Kind::Bass, genre: Genre::House, bars: 2, ..Default::default() };
    let g2 = super::generate_from_spec(&request(Kind::Bass, Genre::House, 2, Scale::new(Key(9), ScaleKind::Minor)), &ctx_for(Genre::House, TimeSignature::default(), 124.), no_chords, 2);
    assert_eq!(g2.source, super::Source::Model);
    assert!(g2.draft.notes.len() >= 4 && g2.draft.notes.iter().all(|n| matches!(n.key % 12, 9 | 0 | 4 | 7)), "{:?}", g2.draft.notes.iter().map(|n| n.key).collect::<Vec<_>>());
    // A spec with nothing in it says so too; a clean one has no notice.
    let empty = super::generate_from_spec(&r, &ctx, MidiSpec::default(), 5);
    assert!(empty.fallback.as_deref().unwrap_or_default().contains("nothing usable"));
    let clean = super::generate_from_spec(&r, &ctx, super::generate(&r, &ctx, 1).spec, 5);
    assert_eq!((clean.source, clean.fallback), (super::Source::Model, None));
}

// ───────────────────────────── For tuning by ear ─────────────────────────────

/// Prints a part as text: `cargo test -p aitrack-ai show_ -- --ignored --nocapture`.
fn show(kind: Kind, genre: Genre, scale: Scale, seed: u64, bars: u32) {
    let ctx = ctx_for(genre, TimeSignature::default(), 124.);
    let g = generate(&request(kind, genre, bars, scale), &ctx, seed);
    println!("\n== {} (seed {seed}) swing {:.0} %  fixes {:?}  warnings {:?}", g.interpretation, g.report.stats.swing * 100., g.report.fixes, g.report.warnings);
    println!("chords: {}", g.spec.chords.iter().map(|c| super::harmony::resolve(c, Parent::of(scale)).name()).collect::<Vec<_>>().join(" "));
    for n in &g.draft.notes {
        println!("  bar {} step {:>5.2}  {:?} {:>3} len {:>4} vel {:>3.0}{}", n.bar(&g.draft.grid), (n.start - n.bar(&g.draft.grid) as Tick * g.draft.grid.bar_ticks) as f32 / 240., n.role, n.key, n.length, n.velocity * 127., if n.declared == super::draft::Declared::No { "" } else { " (declared)" });
    }
}

#[test]
#[ignore]
fn show_examples() {
    let am = Scale::new(Key(9), ScaleKind::Minor);
    show(Kind::Chords, Genre::DeepHouse, am, 3, 4);
    show(Kind::Bass, Genre::TechHouse, am, 5, 2);
    show(Kind::Drums, Genre::House, am, 1, 1);
    show(Kind::Arp, Genre::Trance, am, 2, 2);
    show(Kind::Melody, Genre::House, am, 2, 4);
    show(Kind::Bass, Genre::Techno, am, 11, 1);
    show(Kind::Bass, Genre::Trap, am, 4, 2);
    show(Kind::Drums, Genre::Dnb, am, 6, 4);
    show(Kind::Chords, Genre::House, Scale::new(Key(0), ScaleKind::Major), 9, 4);
}

/// The numbers quoted in the report.
#[test]
#[ignore]
fn print_numbers() {
    let (mut gens, mut notes, mut fixes) = (0usize, 0usize, 0usize);
    let (mut trans, mut smooth, mut maxjump) = (0, 0, 0);
    let (mut mel_int, mut mel_ok) = (0, 0);
    let (mut hat_sd, mut hat_n) = (0f32, 0);
    let mut max_fix_rate = 0f32;
    sweep(|_, _, kind, g, _| {
        gens += 1;
        notes += g.draft.notes.len();
        let f = g.report.total_fixes();
        fixes += f;
        max_fix_rate = max_fix_rate.max(f as f32 / g.draft.notes.len().max(1) as f32);
        if kind == Kind::Chords {
            for w in voicings(g).windows(2) {
                let j = (*w[1].last().unwrap() as i32 - *w[0].last().unwrap() as i32).abs();
                trans += 1;
                smooth += (j <= 5) as usize;
                maxjump = maxjump.max(j);
            }
        }
        if kind == Kind::Melody {
            let mut v: Vec<_> = g.draft.of_role(Role::Lead).map(|n| (n.start, n.key as i32)).collect();
            v.sort();
            for w in v.windows(2) {
                mel_int += 1;
                mel_ok += ((w[1].1 - w[0].1).abs() <= 2) as usize;
            }
        }
        if kind == Kind::Drums {
            let h: Vec<f32> = g.draft.of_role(Role::Hat).map(|n| n.velocity).collect();
            if h.len() > 3 {
                let m = h.iter().sum::<f32>() / h.len() as f32;
                hat_sd += (h.iter().map(|x| (x - m).powi(2)).sum::<f32>() / h.len() as f32).sqrt();
                hat_n += 1;
            }
        }
    });
    println!("generations {gens}, notes {notes}, repairs {fixes} ({:.2} % of notes), worst part {:.0} % repaired", 100. * fixes as f32 / notes as f32, 100. * max_fix_rate);
    println!("chord transitions {trans}: top voice <= 5 semitones in {:.1} % , max jump {maxjump}", 100. * smooth as f32 / trans as f32);
    println!("melody intervals {mel_int}: <= 2 semitones in {:.1} %", 100. * mel_ok as f32 / mel_int as f32);
    println!("hat velocity sd averages {:.3} over {hat_n} drum parts", hat_sd / hat_n as f32);
    let mut same = 0;
    let mut pairs = 0;
    for genre in Genre::ALL {
        for kind in Kind::ALL {
            for i in 0..20u64 {
                let scale = Scale::new(Key(9), ScaleKind::Minor);
                let ctx = ctx_for(genre, TimeSignature::default(), 124.);
                let a = generate(&request(kind, genre, 4, scale), &ctx, i);
                let b = generate(&request(kind, genre, 4, scale), &ctx, i + 1000);
                let grid = |g: &Generated| g.draft.notes.iter().map(|n| (n.key, n.nominal)).collect::<Vec<_>>();
                pairs += 1;
                same += (grid(&a) == grid(&b)) as usize;
            }
        }
    }
    println!("seed pairs with an identical pattern: {same} of {pairs}");
}
