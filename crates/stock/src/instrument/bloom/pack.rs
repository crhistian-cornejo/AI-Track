//! Coherent packs: one roll grows a whole starting idea, a Bloom scene per
//! part (drums, bass, lead, pad, pluck or keys, FX…), all from one
//! producer's palette.
//!
//! ```text
//! seed (+ style, + key) ─► PackCharacter: brightness, wave family, FX
//!                          flavour (dry / plate / hall / chorus / dist),
//!                          movement, analog drift vs digital, width, time,
//!                          a shared delay, a drum species and drum seed
//!       per tonal kind ─► that kind's scene Character taken from the pack
//!                          (its lineage parent still drawn per kind), each
//!                          core imprinted with the pack's drift, cutoff
//!                          place and distortion, then the kind's rules
//!       drums ─► the species and the drum seed (out of 48 candidates)
//!                whose scene character is nearest the pack's brightness,
//!                grit and room, and in the pack's key when it has one
//! ```
//!
//! A pack's tonal scenes are their own (salted apart from
//! [`tonal::generate`], which is unchanged for every seed): a saved pack
//! member keeps its patches in the state (`extra.patches`, with the pack's
//! seed as the scene's seed), and a Roll of that device grows an ordinary
//! scene. Pure and deterministic; it allocates (control thread).

use super::genome::{SOUNDS, Scene, Species, quantize, reflect};
use super::grow::{self, GrowOpts};
use super::patch::{Kind, Patch, PatchScene, det_plain, layout};
use super::rng::{Rng, part};
use super::tonal::{self, Character, KindPriors, priors, waves};
use crate::instrument::wavetable::p as w;

/// Salt of the pack path in seeds (so a pack's bass is not the bass of the
/// same seed's ordinary scene).
const PACK_SALT: u64 = 0x9AC4_0000;

/// A leaning the pack may be asked for; `None` lets the seed choose.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Style {
    Dark,
    Bright,
    Warm,
    Digital,
    Gritty,
    Lush,
}

impl Style {
    pub const ALL: [Style; 6] = [Style::Dark, Style::Bright, Style::Warm, Style::Digital, Style::Gritty, Style::Lush];

    pub fn label(self) -> &'static str {
        match self {
            Style::Dark => "Dark",
            Style::Bright => "Bright",
            Style::Warm => "Warm",
            Style::Digital => "Digital",
            Style::Gritty => "Gritty",
            Style::Lush => "Lush",
        }
    }
}

/// The pack's effects: what every member's room and distortion lean to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Flavour {
    Dry,
    Plate,
    Hall,
    Chorus,
    Dist,
}

impl Flavour {
    pub const ALL: [Flavour; 5] = [Flavour::Dry, Flavour::Plate, Flavour::Hall, Flavour::Chorus, Flavour::Dist];

    /// `(reverb mix, reverb decay, chorus)` as shares of each kind's band.
    fn room(self) -> (f32, f32, f32) {
        match self {
            Flavour::Dry => (0.06, 0.12, 0.0),
            Flavour::Plate => (0.45, 0.3, 0.08),
            Flavour::Hall => (0.82, 0.88, 0.12),
            Flavour::Chorus => (0.32, 0.45, 0.8),
            Flavour::Dist => (0.22, 0.3, 0.0),
        }
    }
}

/// Wave families: factory `WAVE`s that sound related, in order of
/// preference (each kind takes the first its own list allows).
pub const FAMILIES: [(&str, &[usize]); 5] = [
    ("Analog", &[1, 13, 2, 4]),
    ("Digital", &[7, 9, 4, 14]),
    ("Glass", &[5, 14, 10, 6, 8, 2]),
    ("Vocal", &[3, 6, 2, 8]),
    ("Dirty", &[11, 8, 9, 12, 13]),
];

/// What every member of a pack shares.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PackCharacter {
    pub style: Option<Style>,
    /// −1..1: where in each kind's cutoff band the members sit.
    pub bright: f32,
    /// Index into [`FAMILIES`].
    pub family: usize,
    pub flavour: Flavour,
    /// 0..1: how much the members' modulation moves.
    pub movement: f32,
    /// 0 (clean digital) .. 1 (drifting analog): Drift in each kind's band.
    pub analog: f32,
    /// Unison detune scale.
    pub width: f32,
    /// Decay and release scale.
    pub time: f32,
    /// 0 = no delay, else its share of each kind's delay band.
    pub delay: f32,
    /// `DELAY_TIME` choice.
    pub delay_time: f32,
    /// The drum member: species and the seed of its scene (≤ `MAX_SEED`).
    pub species: Species,
    pub drum_seed: u64,
}

fn style_range(style: Option<Style>, r: &mut Rng) -> (f32, usize, Flavour, f32, f32) {
    let bright = match style {
        Some(Style::Dark) => r.between(-0.95, -0.4),
        Some(Style::Bright) => r.between(0.4, 0.95),
        Some(Style::Warm) => r.between(-0.5, 0.05),
        Some(Style::Digital) => r.between(0.0, 0.7),
        _ => r.between(-0.85, 0.85),
    };
    let family = match style {
        Some(Style::Warm) => r.pick(&[0usize, 3]),
        Some(Style::Digital) => r.pick(&[1usize, 2]),
        Some(Style::Gritty) => r.pick(&[4usize, 1, 0]),
        Some(Style::Bright) => r.pick(&[2usize, 1, 0]),
        _ => r.below(FAMILIES.len()),
    };
    let flavour = match style {
        Some(Style::Gritty) => Flavour::Dist,
        Some(Style::Lush) => r.pick(&[Flavour::Hall, Flavour::Chorus]),
        Some(Style::Dark) => r.pick(&[Flavour::Hall, Flavour::Plate, Flavour::Dry]),
        Some(Style::Digital) => r.pick(&[Flavour::Dry, Flavour::Plate, Flavour::Dist]),
        Some(Style::Warm) => r.pick(&[Flavour::Chorus, Flavour::Plate]),
        _ => r.pick(&Flavour::ALL),
    };
    let analog = match style {
        Some(Style::Digital) => r.between(0.0, 0.15),
        Some(Style::Warm) => r.between(0.6, 1.0),
        _ => r.unit(),
    };
    let movement = match style {
        Some(Style::Lush) => r.between(0.6, 1.0),
        _ => r.between(0.15, 1.0),
    };
    (bright, family, flavour, analog, movement)
}

impl PackCharacter {
    /// The character of pack `seed`, leaning to `style`; `key` (root pitch
    /// class, minor) picks drums whose tuned sounds sit in that key.
    pub fn of(seed: u64, style: Option<Style>, key: Option<(u8, bool)>) -> Self {
        let mut r = Rng::new(part(seed, 97, PACK_SALT));
        let (bright, family, flavour, analog, movement) = style_range(style, &mut r);
        let width = r.between(0.75, 1.25) * if style == Some(Style::Lush) { 1.15 } else { 1.0 };
        let time = super::genome::det_exp2(r.between(-0.4, 0.4));
        let delay = if r.chance(0.5) { r.between(0.25, 0.7) } else { 0.0 };
        let delay_time = r.pick(&[2.0, 3.0, 5.0]);
        let species = if r.chance(0.55) {
            Species::Kit
        } else {
            match FAMILIES[family].0 {
                "Digital" => r.pick(&[Species::Glitch, Species::Mixed]),
                "Glass" => r.pick(&[Species::Metallic, Species::Tonal]),
                "Vocal" => r.pick(&[Species::Tonal, Species::Mixed]),
                "Dirty" => r.pick(&[Species::Glitch, Species::Mixed]),
                _ => r.pick(&[Species::Kit, Species::Mixed]),
            }
        };
        let mut ch = Self { style, bright, family, flavour, movement, analog, width, time, delay, delay_time, species, drum_seed: 0 };
        ch.drum_seed = drum_seed(seed, &ch, key);
        ch
    }

    /// The grit the pack's drums and tonal kinds lean to (0..1).
    pub fn grit(&self) -> f32 {
        if self.flavour == Flavour::Dist { 0.7 } else { 0.12 + 0.2 * (1.0 - self.analog) * 0.5 }
    }

    /// An adjective for the pack ("Velvet", "Neon", "Rusty"…).
    pub fn adjective(&self, seed: u64) -> &'static str {
        let pool: &[&str] = if self.flavour == Flavour::Dist {
            tonal::GRIT
        } else if self.bright > 0.4 {
            tonal::BRIGHT
        } else if self.bright < -0.4 {
            tonal::DARK
        } else if self.flavour == Flavour::Hall || self.flavour == Flavour::Chorus {
            tonal::WIDE
        } else if self.movement > 0.75 {
            tonal::MOVING
        } else {
            tonal::SOFT
        };
        pool[(part(seed, 5, PACK_SALT) % pool.len() as u64) as usize]
    }
}

/// The drum seed nearest the pack: out of 48 candidates, the scene whose
/// character (brightness, grit, ambience; key when asked) is closest.
fn drum_seed(seed: u64, ch: &PackCharacter, key: Option<(u8, bool)>) -> u64 {
    let room = ch.flavour.room().0;
    let mut best = (f32::MAX, 0u64);
    for k in 0..48u64 {
        let s = part(seed, k, PACK_SALT + 3) % (super::MAX_SEED + 1);
        let d = grow::scene_character(s, ch.species);
        let mut score = (d.bright - ch.bright * 0.5).abs() + (d.grit - ch.grit()).abs() + 2.0 * (d.space - (0.1 + 0.35 * room)).abs();
        if let Some((root, minor)) = key {
            if d.root != i32::from(root % 12) {
                score += 0.8;
            }
            if d.minor != minor {
                score += 0.3;
            }
        }
        if score < best.0 {
            best = (score, s);
        }
    }
    best.1
}

/// The plain value at share `f` (0..1) of `id`'s band in a kind.
fn band(kp: &KindPriors, id: u32, f: f32) -> f32 {
    let Some(s) = Patch::slot(id) else { return 0.0 };
    det_plain(&layout().infos[s], kp.lo[s] + (kp.hi[s] - kp.lo[s]) * f.clamp(0.0, 1.0))
}

/// Where a patch's value of `id` sits in its kind's band, 0..1.
fn share(kp: &KindPriors, x: &Patch, id: u32) -> f32 {
    let Some(s) = Patch::slot(id) else { return 0.0 };
    let span = kp.hi[s] - kp.lo[s];
    if span < 1e-6 { 0.5 } else { ((reflect(x.v[s]) - kp.lo[s]) / span).clamp(0.0, 1.0) }
}

/// The wave of `family` a kind can use (0 = the kind's own choice).
pub fn family_wave(kind: Kind, family: usize) -> usize {
    let list = waves(kind);
    FAMILIES[family.min(FAMILIES.len() - 1)].1.iter().copied().find(|v| list.contains(v)).unwrap_or(0)
}

/// A kind's scene character, taken from the pack.
fn kind_character(kind: Kind, pc: &PackCharacter, r: &mut Rng) -> Character {
    let kp = priors().kind(kind);
    let mut ch = Character::of(kind, r);
    let wave = family_wave(kind, pc.family);
    if wave > 0 {
        ch.wave = wave;
    }
    ch.bright = pc.bright * 0.6;
    ch.width = pc.width;
    ch.grit = if pc.flavour == Flavour::Dist { r.between(0.6, 0.9) } else { ch.grit * 0.3 };
    ch.motion = 0.25 + 0.75 * pc.movement;
    ch.time = pc.time;
    let (rv, rd, chorus) = pc.flavour.room();
    ch.reverb = band(kp, w::REVERB_MIX, rv);
    ch.reverb_decay = band(kp, w::REVERB_DECAY, rd);
    ch.chorus = if chorus > 0.0 { band(kp, w::CHORUS_MIX, chorus) } else { 0.0 };
    ch.delay = if pc.delay > 0.0 && !matches!(kind, Kind::Bass) { band(kp, w::DELAY_MIX, pc.delay) } else { 0.0 };
    ch.delay_time = pc.delay_time;
    ch.delay_fb = band(kp, w::DELAY_FEEDBACK, 0.35);
    ch
}

/// Press the pack's shared traits into a core: the family's wave (on most
/// cells), drift, the cutoff's place in the kind's band, the distortion;
/// then the kind's rules.
fn imprint(x: &mut Patch, pc: &PackCharacter, r: &mut Rng) {
    let kind = x.kind;
    let kp = priors().kind(kind);
    let wave = family_wave(kind, pc.family);
    if wave > 0 && x.choice(w::osc(0, w::WAVE)) != wave && r.chance(0.6) {
        x.set_plain(w::osc(0, w::WAVE), wave as f32);
    }
    x.set_plain(w::DRIFT, band(kp, w::DRIFT, 0.04 + 0.86 * pc.analog));
    if let Some(s) = Patch::slot(w::CUTOFF) {
        let target = kp.lo[s] + (kp.hi[s] - kp.lo[s]) * (0.5 + 0.38 * pc.bright);
        x.v[s] = quantize(0.4 * reflect(x.v[s]) + 0.6 * target);
    }
    let dist_ok = !matches!(kind, Kind::Pad | Kind::Texture);
    if pc.flavour == Flavour::Dist && dist_ok {
        if x.choice(w::DIST_MODE) == 0 {
            x.set_plain(w::DIST_MODE, 1.0);
        }
        x.set_plain(w::DIST_DRIVE, band(kp, w::DIST_DRIVE, 0.6));
        x.set_plain(w::DIST_MIX, 100.0);
    } else if pc.flavour != Flavour::Dist {
        let d = x.plain(w::DIST_DRIVE);
        x.set_plain(w::DIST_DRIVE, d * 0.5);
    }
    tonal::constrain(x);
}

/// What a pack shares, measured on one patch, each 0..1 within the
/// patch's kind: cutoff, drift, reverb mix and decay, chorus, distortion
/// and modulation.
pub fn traits(p: &Patch) -> [f32; 7] {
    let x = p.folded();
    let kp = priors().kind(x.kind);
    let dist = if x.choice(w::DIST_MODE) > 0 { (x.plain(w::DIST_DRIVE) / 50.0).clamp(0.0, 1.0) * (x.plain(w::DIST_MIX) / 100.0) } else { 0.0 };
    [share(kp, &x, w::CUTOFF), share(kp, &x, w::DRIFT), share(kp, &x, w::REVERB_MIX), share(kp, &x, w::REVERB_DECAY), share(kp, &x, w::CHORUS_MIX), dist, tonal::motion(&x)]
}

/// Distance between two trait vectors (Euclidean).
pub fn trait_distance(a: &[f32; 7], b: &[f32; 7]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y) * (x - y)).sum::<f32>().sqrt()
}

/// The tonal scene of `kind` in pack `seed` (its scene seed is the pack's).
pub fn tonal_scene(seed: u64, pc: &PackCharacter, kind: Kind) -> PatchScene {
    let mut out = PatchScene::empty(kind);
    out.seed = seed;
    if !kind.is_tonal() {
        return out;
    }
    let salt = kind.salt() ^ PACK_SALT;
    let mut r = Rng::new(part(seed, 98, salt));
    let ch = kind_character(kind, pc, &mut r);
    for i in 0..SOUNDS {
        let mut rr = Rng::new(part(seed, i as u64, salt + 1));
        let mut c = tonal::core(kind, &ch, &mut rr);
        imprint(&mut c, pc, &mut rr);
        out.cores[i] = c;
        out.paths[i] = part(seed, i as u64, salt + 2);
        out.distances[i] = quantize(rr.between(0.05, 0.3));
        out.patches[i] = tonal::grow(&c, out.paths[i], out.distances[i]);
    }
    out
}

/// The cell of a tonal scene that best carries the pack: nearest the
/// pack's own traits.
fn best_cell(scene: &PatchScene, pc: &PackCharacter) -> usize {
    let (rv, rd, chorus) = pc.flavour.room();
    let dist = if pc.flavour == Flavour::Dist && !matches!(scene.kind, Kind::Pad | Kind::Texture) { 0.7 } else { 0.0 };
    let want = [0.5 + 0.38 * pc.bright, 0.04 + 0.86 * pc.analog, rv, rd, chorus, dist];
    let wave = family_wave(scene.kind, pc.family);
    let mut best = (f32::MAX, 0);
    for (i, p) in scene.patches.iter().enumerate() {
        let t = traits(p);
        let off_family = wave > 0 && !(0..3).any(|o| p.plain(w::osc(o, w::LEVEL)) >= 20.0 && p.choice(w::osc(o, w::WAVE)) == wave);
        let d: f32 = want.iter().zip(&t).map(|(a, b)| (a - b) * (a - b)).sum::<f32>() + if off_family { 0.3 } else { 0.0 };
        if d < best.0 {
            best = (d, i);
        }
    }
    best.1
}

/// What a member plays.
#[derive(Clone, Debug, PartialEq)]
pub enum Sounds {
    Drums(Box<Scene>),
    Tonal(Box<PatchScene>),
}

/// One part of a pack: a Bloom of `kind`.
#[derive(Clone, Debug, PartialEq)]
pub struct Member {
    pub kind: Kind,
    pub sounds: Sounds,
    /// The cell the device selects (the pack's best fit; drums play all).
    pub selected: usize,
    /// "Velvet Growl", "Neon Kit".
    pub name: String,
}

impl Member {
    /// The patch it plays (tonal members).
    pub fn patch(&self) -> Option<&Patch> {
        match &self.sounds {
            Sounds::Tonal(s) => s.patches.get(self.selected),
            Sounds::Drums(_) => None,
        }
    }

    /// The seed its device's Seed parameter holds.
    pub fn seed(&self) -> u64 {
        match &self.sounds {
            Sounds::Tonal(s) => s.seed,
            Sounds::Drums(s) => s.seed,
        }
    }
}

/// A grown pack.
#[derive(Clone, Debug, PartialEq)]
pub struct Pack {
    /// The pack's seed (≤ `MAX_SEED`; the tonal members' scene seed).
    pub seed: u64,
    pub character: PackCharacter,
    pub members: Vec<Member>,
}

/// The default members: drums, bass, lead, pad, pluck, FX.
pub const DEFAULT_KINDS: [Kind; 6] = [Kind::Drums, Kind::Bass, Kind::Lead, Kind::Pad, Kind::Pluck, Kind::Fx];

fn species_noun(s: Species) -> &'static str {
    match s {
        Species::Kit => "Kit",
        Species::Tonal => "Perc",
        Species::Metallic => "Metals",
        Species::Glitch => "Glitch Kit",
        Species::Mixed => "Hybrid Kit",
    }
}

/// Grow pack `seed` with `character` (from [`PackCharacter::of`]): one
/// member per kind in `kinds`, in that order (a kind listed twice grows
/// once).
pub fn pack(seed: u64, character: &PackCharacter, kinds: &[Kind]) -> Pack {
    let seed = seed.min(super::MAX_SEED);
    let mut members: Vec<Member> = Vec::with_capacity(kinds.len());
    for &kind in kinds {
        if members.iter().any(|m| m.kind == kind) {
            continue;
        }
        let m = if kind.is_tonal() {
            let scene = tonal_scene(seed, character, kind);
            let selected = best_cell(&scene, character);
            let name = tonal::scene_names(&scene)[selected].clone();
            Member { kind, sounds: Sounds::Tonal(Box::new(scene)), selected, name }
        } else {
            let scene = grow::generate(character.drum_seed, GrowOpts { species: character.species, spread: 1.0 });
            let name = format!("{} {}", character.adjective(seed), species_noun(character.species));
            Member { kind, sounds: Sounds::Drums(Box::new(scene)), selected: 0, name }
        };
        members.push(m);
    }
    Pack { seed, character: *character, members }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packs_are_deterministic_and_seeds_differ() {
        let ch = PackCharacter::of(4242, None, Some((9, true)));
        assert_eq!(ch, PackCharacter::of(4242, None, Some((9, true))));
        let a = pack(4242, &ch, &DEFAULT_KINDS);
        let b = pack(4242, &ch, &DEFAULT_KINDS);
        assert_eq!(a, b);
        assert_eq!(a.members.len(), 6);
        assert_eq!(a.members.iter().map(|m| m.kind).collect::<Vec<_>>(), DEFAULT_KINDS.to_vec());
        let other = pack(4243, &PackCharacter::of(4243, None, None), &DEFAULT_KINDS);
        assert_ne!(a.members[1], other.members[1]);
        for m in &a.members {
            match &m.sounds {
                Sounds::Tonal(s) => {
                    assert_eq!(s.kind, m.kind);
                    assert_eq!(s.seed, 4242);
                    assert!(s.patches.iter().all(|p| p.kind == m.kind));
                }
                Sounds::Drums(s) => {
                    assert_eq!(s.seed, ch.drum_seed);
                    assert_eq!(s.species, ch.species);
                    assert!(s.seed <= super::super::MAX_SEED);
                }
            }
            assert!(!m.name.is_empty());
        }
        // Duplicates grow once; a style leans the character.
        assert_eq!(pack(1, &PackCharacter::of(1, None, None), &[Kind::Bass, Kind::Bass]).members.len(), 1);
        for seed in 0..20 {
            assert!(PackCharacter::of(seed, Some(Style::Dark), None).bright < 0.0);
            assert!(PackCharacter::of(seed, Some(Style::Bright), None).bright > 0.0);
            assert_eq!(PackCharacter::of(seed, Some(Style::Gritty), None).flavour, Flavour::Dist);
        }
    }

    #[test]
    fn pack_scenes_keep_their_kinds_rules_and_leave_ordinary_seeds_alone() {
        for seed in [3u64, 77, 900] {
            let ch = PackCharacter::of(seed, None, None);
            for kind in Kind::TONAL {
                let s = tonal_scene(seed, &ch, kind);
                assert_ne!(s.patches, tonal::generate(seed, kind).patches, "the pack path is its own");
                for x in &s.patches {
                    let mut y = *x;
                    tonal::constrain(&mut y);
                    assert!(y.distance(x) < 1e-5, "{kind:?}: a pack patch breaks its kind's rules");
                    for (id, lo, hi) in tonal::rules(kind) {
                        let v = x.plain(id);
                        let tol = 1e-3 * (hi - lo).abs().max(1.0) + 1e-3 * v.abs();
                        assert!(v >= lo - tol && v <= hi + tol, "{kind:?} param {id} = {v} outside {lo}..{hi}");
                    }
                }
            }
        }
    }

    /// Members of one pack share their character: their traits (each in its
    /// own kind's band) sit measurably closer to each other than to the
    /// same kinds in other packs.
    #[test]
    fn members_of_one_pack_are_closer_than_members_of_different_packs() {
        let kinds = [Kind::Bass, Kind::Lead, Kind::Pad, Kind::Pluck, Kind::Keys];
        let packs: Vec<Vec<[f32; 7]>> = (0..24u64)
            .map(|seed| {
                let p = pack(seed * 131 + 7, &PackCharacter::of(seed * 131 + 7, None, None), &kinds);
                p.members.iter().map(|m| traits(m.patch().unwrap())).collect()
            })
            .collect();
        let (mut within, mut nw, mut across, mut na) = (0.0f32, 0, 0.0f32, 0);
        for (p, a) in packs.iter().enumerate() {
            for (q, b) in packs.iter().enumerate() {
                for i in 0..kinds.len() {
                    for j in 0..kinds.len() {
                        if i == j {
                            continue;
                        }
                        let d = trait_distance(&a[i], &b[j]);
                        if p == q {
                            within += d;
                            nw += 1;
                        } else {
                            across += d;
                            na += 1;
                        }
                    }
                }
            }
        }
        let (within, across) = (within / nw as f32, across / na as f32);
        assert!(within < across * 0.75, "within a pack {within:.3} vs across packs {across:.3}");
        // Ordinary scenes of the same seeds share less than packs do.
        let loose: Vec<Vec<[f32; 7]>> = (0..24u64).map(|seed| kinds.iter().map(|&k| traits(&tonal::generate(seed * 131 + 7, k).patches[0])).collect()).collect();
        let mut lw = (0.0f32, 0);
        for a in &loose {
            for i in 0..kinds.len() {
                for j in 0..kinds.len() {
                    if i != j {
                        lw.0 += trait_distance(&a[i], &a[j]);
                        lw.1 += 1;
                    }
                }
            }
        }
        assert!(within < lw.0 / lw.1 as f32 * 0.8, "a pack {within:.3} vs unrelated scenes {:.3}", lw.0 / lw.1 as f32);
        // The wave family carries over where the kind allows it.
        for seed in 0..12u64 {
            let ch = PackCharacter::of(seed, None, None);
            let p = pack(seed, &ch, &kinds);
            let on = p.members.iter().filter(|m| m.patch().is_some_and(|x| (0..3).any(|o| x.choice(w::osc(o, w::WAVE)) == family_wave(m.kind, ch.family)))).count();
            assert!(on >= 2, "seed {seed}: only {on} members use the {} family", FAMILIES[ch.family].0);
        }
    }

    #[test]
    fn drums_follow_the_pack_and_its_key() {
        let mut hits = 0;
        for seed in 0..16u64 {
            let ch = PackCharacter::of(seed, None, Some((2, false)));
            let d = grow::scene_character(ch.drum_seed, ch.species);
            if d.root == 2 {
                hits += 1;
            }
        }
        assert!(hits >= 12, "drums in the pack's key: {hits}/16");
    }
}
