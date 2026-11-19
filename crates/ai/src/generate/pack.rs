//! The parts of a Bloom pack: one clip per member, all in one key, over one
//! progression and one groove, so the pack starts as one idea.
//!
//! ```text
//! key, tempo, genre (default from the tempo), bars, seed
//!   └► the progression: the Chords template of the seed (every chord part
//!      of the pack plays it; the others follow it bar by bar)
//!   └► drums on the Bloom kit's pads (sound i on Base + i) ─► the kick map
//!   └► bass (follows the chords, clear of the kick), pad (held chords),
//!      keys (chord stabs), pluck (arpeggio), lead (melody), texture (held
//!      chords), FX (a hit on the first bar, one halfway through long
//!      clips, and a long note over the last bar into the loop)
//! ```
//!
//! Pure: the same request and seed give the same notes. The app turns the
//! parts into tracks and clips.

use aitrack_model::{Key, Scale, ScaleKind, Tick};
use aitrack_stock::instrument::bloom::genome::{Role as Cell, Scene};
use aitrack_stock::instrument::bloom::patch::Kind as Sound;

use super::draft::{Declared, DraftNote};
use super::grid::{Grid, STEP};
use super::harmony::{ChordSym, Parent, resolve};
use super::spec::{GenRequest, Genre, Kind, Pattern, Role};
use super::context::KickMap;
use super::{GenContext, Generated, PadMap, Target, TargetKind};

/// The drum notes a Bloom drum scene plays in Kit mode (sound `i` on
/// `base + i`): each role on the first cell of that role, with the nearest
/// stand-ins (a clap or rim for a missing snare, a tom or blip for perc, a
/// noise or metal cell for the crash).
pub fn bloom_pads(scene: &Scene, base: u8) -> PadMap {
    let first = |roles: &[Cell]| roles.iter().find_map(|r| scene.sounds.iter().position(|s| s.role == *r)).map(|i| base.saturating_add(i as u8));
    PadMap {
        kick: first(&[Cell::Kick]),
        snare: first(&[Cell::Snare, Cell::Clap, Cell::Rim, Cell::Blip, Cell::Tom, Cell::Perc]),
        clap: first(&[Cell::Clap, Cell::Snare]),
        hat: first(&[Cell::HatClosed, Cell::Metal, Cell::Noise]),
        open_hat: first(&[Cell::HatOpen]),
        perc: first(&[Cell::Perc, Cell::Rim, Cell::Tom, Cell::Blip]),
        crash: first(&[Cell::Noise, Cell::Metal, Cell::Zap, Cell::Bell]),
    }
}

/// The genre a tempo suggests when none is asked for (as a request with no
/// genre word reads it).
pub fn genre_for_tempo(bpm: f64) -> Genre {
    match bpm {
        b if b >= 165. => Genre::Dnb,
        b if b <= 95. => Genre::Trap,
        b if (129. ..=140.).contains(&b) => Genre::TechHouse,
        _ => Genre::House,
    }
}

/// What to generate.
#[derive(Clone, Debug)]
pub struct PackRequest {
    /// Bars of every clip (1..=16).
    pub bars: u32,
    /// `None`: from the tempo.
    pub genre: Option<Genre>,
    /// `None`: the context's key, else A minor.
    pub key: Option<Scale>,
    pub seed: u64,
}

impl Default for PackRequest {
    fn default() -> Self {
        Self { bars: 4, genre: None, key: None, seed: 1 }
    }
}

/// One member's clip.
#[derive(Clone, Debug)]
pub struct Part {
    pub sound: Sound,
    /// From tick 0 of the clip.
    pub notes: Vec<DraftNote>,
    pub clip_ticks: Tick,
    /// "Bass · house · A minor · 4 bars".
    pub label: String,
}

/// Every member's clip, and what they share.
#[derive(Clone, Debug)]
pub struct Plan {
    pub scale: Scale,
    pub genre: Genre,
    pub bars: u32,
    /// The chord of each bar (every part follows it).
    pub chords: Vec<ChordSym>,
    pub parts: Vec<Part>,
}

/// The progression a [`Generated`] chord part plays, one chord per bar
/// (the chord sounding at the bar's start).
fn bar_chords(g: &Generated, ctx: &GenContext, scale: Scale, bars: u32) -> Vec<ChordSym> {
    let bpb = ctx.signature.numerator as f32 * 4. / ctx.signature.denominator.max(1) as f32;
    let parent = Parent::of(scale);
    let fallback = ChordSym::new(scale.root.0, if matches!(scale.kind, ScaleKind::Major | ScaleKind::Lydian | ScaleKind::Mixolydian | ScaleKind::MajorPentatonic) { super::spec::Quality::Maj } else { super::spec::Quality::Min }, Vec::new());
    (0..bars)
        .map(|bar| {
            let at = bar as f32 * bpb;
            g.spec.chords.iter().rfind(|c| c.bar as f32 * bpb + c.beat <= at + 1e-3).map_or(fallback.clone(), |c| resolve(c, parent))
        })
        .collect()
}

fn kick_map(g: &Generated, grid: &Grid) -> Option<KickMap> {
    let mut steps = vec![false; (grid.steps_per_bar * grid.bars) as usize];
    let mut any = false;
    for n in g.draft.notes.iter().filter(|n| n.role == Role::Kick) {
        let s = (n.nominal / STEP) as usize;
        if s < steps.len() {
            steps[s] = true;
            any = true;
        }
    }
    any.then_some(KickMap { steps })
}

/// The FX part: a hit on the first bar (and halfway through clips of 8
/// bars or more), then a long note over the last bar, leading into the
/// loop's start. On the key's root, around C3 (the patches' home).
fn fx_notes(scale: Scale, grid: &Grid) -> Vec<DraftNote> {
    let bar = grid.bar_ticks;
    let root = 60 + scale.root.0 % 12;
    let note = |start: Tick, length: Tick, key: u8, velocity: f32| DraftNote { key, start, length, velocity, role: Role::Lead, layer: 0, nominal: start, declared: Declared::No, slide: false };
    let mut out = vec![note(0, bar / 2, root - 12, 0.9)];
    if grid.bars >= 8 {
        out.push(note(bar * (grid.bars / 2) as Tick, bar / 2, root - 12, 0.75));
    }
    if grid.bars >= 2 {
        out.push(note(bar * (grid.bars - 1) as Tick, bar, root, 0.8));
    }
    out
}

/// What a member plays: the request kind and its pattern.
fn request_for(sound: Sound, has_pluck: bool) -> Option<(Kind, Option<Pattern>, f32)> {
    Some(match sound {
        Sound::Drums => (Kind::Drums, None, 0.55),
        Sound::Bass => (Kind::Bass, None, 0.5),
        Sound::Pad | Sound::Texture => (Kind::Chords, Some(Pattern::Whole), 0.2),
        Sound::Keys => (Kind::Chords, if has_pluck { Some(Pattern::StabsOffbeat) } else { None }, 0.5),
        Sound::Pluck => (Kind::Arp, None, 0.45),
        Sound::Lead => (Kind::Melody, None, 0.5),
        Sound::Fx => return None,
    })
}

/// The clips of a pack's members (`sounds`, in order; `drums` is the drum
/// member's scene, for its pads). `base` gives the tempo, the meter and
/// the project's key; its other tracks are not followed (the pack is its
/// own idea).
pub fn plan(sounds: &[Sound], drums: Option<&Scene>, base: &GenContext, req: &PackRequest) -> Plan {
    let bars = req.bars.clamp(1, super::spec::MAX_BARS);
    let scale = req.key.or(base.scale).unwrap_or(Scale::new(Key(9), ScaleKind::Minor));
    let genre = req.genre.unwrap_or_else(|| genre_for_tempo(base.bpm));
    let grid = Grid::new(base.signature, bars);
    let mut ctx = GenContext { scale: Some(scale), bpm: base.bpm, signature: base.signature, ..Default::default() };
    let make = |kind: Kind, pattern: Option<Pattern>, density: f32| {
        let mut r = GenRequest::new(kind);
        r.genre = genre;
        r.bars = bars;
        r.key = Some(scale);
        r.pattern = pattern;
        r.density = density;
        r.seed = Some(req.seed);
        r
    };
    // The progression every part shares.
    let progression = super::generate(&make(Kind::Chords, Some(Pattern::Whole), 0.2), &ctx, req.seed);
    let chords = bar_chords(&progression, &ctx, scale, bars);
    ctx.harmony = chords.iter().cloned().map(Some).collect();
    // The groove: the drums first, so the bass clears the kick.
    let mut done: Vec<(Sound, Generated)> = Vec::new();
    if sounds.contains(&Sound::Drums) {
        let pads = drums.map_or(PadMap::gm(), |s| bloom_pads(s, 36));
        let dctx = GenContext { target: Target { track: None, name: "Bloom Drums".into(), kind: TargetKind::Drums, pads }, ..ctx.clone() };
        let g = super::generate(&make(Kind::Drums, None, 0.55), &dctx, req.seed);
        ctx.kick = kick_map(&g, &grid);
        done.push((Sound::Drums, g));
    }
    let has_pluck = sounds.contains(&Sound::Pluck);
    let mut parts = Vec::with_capacity(sounds.len());
    for &sound in sounds {
        if parts.iter().any(|p: &Part| p.sound == sound) {
            continue;
        }
        let g = match done.iter().position(|(s, _)| *s == sound) {
            Some(i) => Some(done.swap_remove(i).1),
            None => request_for(sound, has_pluck).map(|(kind, pattern, density)| super::generate(&make(kind, pattern, density), &ctx, req.seed)),
        };
        let part = match g {
            Some(g) => Part { sound, notes: g.draft.notes, clip_ticks: g.clip_ticks, label: g.interpretation },
            None => Part { sound, notes: fx_notes(scale, &grid), clip_ticks: grid.clip_ticks(), label: format!("FX · {} · {} · {bars} bars", genre.label(), scale.display()) },
        };
        parts.push(part);
    }
    Plan { scale, genre, bars, chords, parts }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitrack_stock::instrument::bloom::genome::Species;
    use aitrack_stock::instrument::bloom::grow::{GrowOpts, generate};

    const ALL: [Sound; 6] = [Sound::Drums, Sound::Bass, Sound::Lead, Sound::Pad, Sound::Pluck, Sound::Fx];

    fn in_key_or_chord(scale: Scale, chords: &[ChordSym], bar_ticks: Tick, n: &DraftNote) -> bool {
        let pc = n.key % 12;
        let chord = &chords[((n.start / bar_ticks) as usize).min(chords.len() - 1)];
        scale.contains(n.key) || chord.pcs().contains(&pc)
    }

    #[test]
    fn a_plan_is_one_idea_in_one_key() {
        let scene = generate(321, GrowOpts { species: Species::Kit, spread: 1.0 });
        let base = GenContext { bpm: 124., ..Default::default() };
        let req = PackRequest { bars: 4, key: Some(Scale::new(Key(2), ScaleKind::Minor)), seed: 77, ..Default::default() };
        let plan = plan(&ALL, Some(&scene), &base, &req);
        assert_eq!(plan.genre, Genre::House, "from the tempo");
        assert_eq!(plan.parts.iter().map(|p| p.sound).collect::<Vec<_>>(), ALL.to_vec());
        assert_eq!(plan.chords.len(), 4);
        let bar = Grid::new(base.signature, 4).bar_ticks;
        let pads = bloom_pads(&scene, 36).keys();
        for p in &plan.parts {
            assert!(!p.notes.is_empty(), "{:?} plays", p.sound);
            assert_eq!(p.clip_ticks, bar * 4);
            for n in &p.notes {
                assert!(n.start >= 0 && n.start < p.clip_ticks, "{:?} note inside the clip", p.sound);
                if p.sound == Sound::Drums {
                    assert!(pads.contains(&n.key), "drum note {} on a Bloom pad", n.key);
                } else {
                    assert!(in_key_or_chord(plan.scale, &plan.chords, bar, n), "{:?} note {} outside D minor and its chord", p.sound, n.key);
                }
            }
        }
        // Deterministic.
        let again = super::plan(&ALL, Some(&scene), &base, &req);
        for (a, b) in plan.parts.iter().zip(&again.parts) {
            assert_eq!(a.notes, b.notes);
        }
        // The pad holds the progression the others follow.
        let pad = plan.parts.iter().find(|p| p.sound == Sound::Pad).unwrap();
        for (bar_i, chord) in plan.chords.iter().enumerate() {
            let at = bar_i as Tick * bar;
            let held: Vec<u8> = pad.notes.iter().filter(|n| n.nominal <= at && n.nominal + n.length > at).map(|n| n.key % 12).collect();
            assert!(held.contains(&chord.root), "bar {bar_i}: the pad holds {:?} over {:?}", held, chord.root);
        }
    }

    #[test]
    fn bloom_kits_map_every_role_to_a_pad() {
        for species in Species::ALL {
            let s = generate(5, GrowOpts { species, spread: 1.0 });
            let m = bloom_pads(&s, 36);
            assert!(m.kick.is_some() && m.snare.is_some() && m.hat.is_some(), "{species:?}: {m:?}");
            assert!(m.keys().iter().all(|k| (36..52).contains(k)));
        }
        let kit = bloom_pads(&generate(5, GrowOpts::default()), 36);
        assert_eq!((kit.kick, kit.snare, kit.clap, kit.hat, kit.open_hat), (Some(36), Some(38), Some(39), Some(42), Some(46)), "a Kit scene lays out like GM");
        assert_eq!(genre_for_tempo(174.), Genre::Dnb);
        assert_eq!(genre_for_tempo(132.), Genre::TechHouse);
    }

    #[test]
    fn fx_hits_land_on_bar_boundaries() {
        let base = GenContext::default();
        let plan = plan(&[Sound::Fx], None, &base, &PackRequest { bars: 8, ..Default::default() });
        let bar = Grid::new(base.signature, 8).bar_ticks;
        let fx = &plan.parts[0];
        assert_eq!(fx.notes.len(), 3);
        assert!(fx.notes.iter().all(|n| n.start % bar == 0 && n.end() <= fx.clip_ticks));
        assert_eq!(fx.notes.last().unwrap().end(), fx.clip_ticks, "the last one runs into the loop");
    }
}
