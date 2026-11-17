//! Harmony: chords with root, alteration, quality and extensions, the
//! 7-note parent scale they are built on, voicing candidates and the
//! voice-leading search (dynamic programming over a cost, section 5.3 of
//! docs/research/22-generador-midi.md).

use aitrack_model::{Key, Scale, ScaleKind};

use super::rng::Rng;
use super::spec::{ChordSpec, Quality};
use super::tables::{CHORD_BOTTOM, CHORD_CENTRE, CHORD_TOP};

// ───────────────────────────── Parent scale ─────────────────────────────

/// A scale of exactly seven notes that chords are stacked on. Pentatonic
/// and blues scales use their natural major or minor; the chromatic scale
/// (no key at all) takes the natural minor, the usual colour of the genres
/// here. It depends on the scale only, so a template and `realize` always
/// agree on what a degree means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Parent {
    pub root: u8,
    pub kind: ScaleKind,
}

impl Parent {
    pub fn of(scale: Scale) -> Self {
        let kind = match scale.kind {
            ScaleKind::MajorPentatonic => ScaleKind::Major,
            ScaleKind::MinorPentatonic | ScaleKind::Blues | ScaleKind::Chromatic => ScaleKind::Minor,
            k => k,
        };
        Self { root: scale.root.0 % 12, kind }
    }

    pub fn scale(self) -> Scale {
        Scale::new(Key(self.root), self.kind)
    }

    pub fn intervals(self) -> &'static [u8] {
        self.kind.intervals()
    }

    /// Semitones above the root of scale degree `d` (any integer, wraps).
    pub fn degree_iv(self, d: i32) -> i32 {
        let iv = self.intervals();
        let n = iv.len() as i32;
        d.div_euclid(n) * 12 + iv[d.rem_euclid(n) as usize] as i32
    }

    pub fn degree_pc(self, d: i32) -> u8 {
        (self.root as i32 + self.degree_iv(d)).rem_euclid(12) as u8
    }

    pub fn contains_pc(self, pc: u8) -> bool {
        self.scale().mask() & (1 << (pc % 12)) != 0
    }

    /// The degree 0..=6 whose pitch class is closest to `pc`, with the
    /// alteration (-1, 0, +1) that reaches it, preferring no alteration.
    pub fn degree_of(self, pc: u8) -> (u8, i32) {
        let mut best = (0u8, 0i32, i32::MAX);
        for d in 0..7 {
            let diff = (pc as i32 - self.degree_pc(d) as i32 + 18).rem_euclid(12) - 6;
            let cost = diff.abs() * 10 + (diff != 0) as i32;
            if diff.abs() <= 1 && cost < best.2 {
                best = (d as u8, diff, cost);
            }
        }
        if best.2 == i32::MAX {
            // Two semitones from every degree cannot happen in 7 notes; keep the nearest anyway.
            let d = (0..7).min_by_key(|d| ((pc as i32 - self.degree_pc(*d) as i32 + 18).rem_euclid(12) - 6).abs()).unwrap_or(0);
            return (d as u8, 0);
        }
        (best.0, best.1)
    }
}

// ───────────────────────────── Chords ─────────────────────────────

/// The tones of each quality above the root, in semitones.
pub fn quality_intervals(q: Quality) -> &'static [u8] {
    match q {
        Quality::Maj => &[0, 4, 7],
        Quality::Min => &[0, 3, 7],
        Quality::Dim => &[0, 3, 6],
        Quality::Aug => &[0, 4, 8],
        Quality::Sus2 => &[0, 2, 7],
        Quality::Sus4 => &[0, 5, 7],
        Quality::Maj7 => &[0, 4, 7, 11],
        Quality::Min7 => &[0, 3, 7, 10],
        Quality::Dom7 => &[0, 4, 7, 10],
        Quality::Min9 => &[0, 3, 7, 10, 14],
        Quality::Maj9 => &[0, 4, 7, 11, 14],
        Quality::Dom9 => &[0, 4, 7, 10, 14],
        Quality::Add9 => &[0, 4, 7, 14],
        Quality::Min6 => &[0, 3, 7, 9],
        Quality::Min11 => &[0, 3, 7, 10, 14, 17],
        Quality::Dim7 => &[0, 3, 6, 9],
        Quality::M7b5 => &[0, 3, 6, 10],
    }
}

fn suffix(q: Quality) -> &'static str {
    match q {
        Quality::Maj => "",
        Quality::Min => "m",
        Quality::Dim => "dim",
        Quality::Aug => "aug",
        Quality::Sus2 => "sus2",
        Quality::Sus4 => "sus4",
        Quality::Maj7 => "maj7",
        Quality::Min7 => "m7",
        Quality::Dom7 => "7",
        Quality::Min9 => "m9",
        Quality::Maj9 => "maj9",
        Quality::Dom9 => "9",
        Quality::Add9 => "add9",
        Quality::Min6 => "m6",
        Quality::Min11 => "m11",
        Quality::Dim7 => "dim7",
        Quality::M7b5 => "m7b5",
    }
}

fn has_major_third(q: Quality) -> bool {
    matches!(q, Quality::Maj | Quality::Aug | Quality::Maj7 | Quality::Dom7 | Quality::Maj9 | Quality::Dom9 | Quality::Add9)
}

fn is_dominant(q: Quality) -> bool {
    matches!(q, Quality::Dom7 | Quality::Dom9)
}

/// The intervals an extension word adds, after dropping the ones that clash
/// with the chord: an eleventh over a major third, a flat ninth outside a
/// dominant, a thirteenth over a flat sixth or a diminished chord...
pub fn resolve_ext(q: Quality, words: &[String]) -> Vec<u8> {
    let core = quality_intervals(q);
    let mut out: Vec<u8> = Vec::new();
    for w in words {
        let iv = match w.as_str() {
            "9" if !matches!(q, Quality::Sus2 | Quality::Add9) => 14,
            "b9" if is_dominant(q) => 13,
            "11" if !has_major_third(q) && !matches!(q, Quality::Sus4 | Quality::Dim | Quality::Dim7) => 17,
            "#11" if matches!(q, Quality::Maj7 | Quality::Maj9 | Quality::Dom7 | Quality::Dom9) => 18,
            "13" if matches!(q, Quality::Min7 | Quality::Min9 | Quality::Dom7 | Quality::Dom9 | Quality::Maj7 | Quality::Maj9) => 21,
            "b13" if is_dominant(q) => 20,
            _ => continue,
        };
        let pc = iv % 12;
        if core.iter().any(|c| c % 12 == pc) || out.iter().any(|c| c % 12 == pc) {
            continue;
        }
        // Never a semitone above another tone (b9 on a dominant is the one wanted clash).
        let clash = core.iter().chain(out.iter()).any(|c| (pc + 12 - c % 12) % 12 == 1) && iv != 13;
        if !clash {
            out.push(iv);
        }
    }
    out
}

/// A chord ready to voice: absolute root pitch class and the tones above it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChordSym {
    pub root: u8,
    pub quality: Quality,
    /// Extension intervals that survived [`resolve_ext`].
    pub ext: Vec<u8>,
}

impl ChordSym {
    pub fn new(root: u8, quality: Quality, ext: Vec<u8>) -> Self {
        Self { root: root % 12, quality, ext }
    }

    /// Every tone as semitones above the root, in stacking order.
    pub fn tones(&self) -> Vec<u8> {
        let mut t = quality_intervals(self.quality).to_vec();
        t.extend(&self.ext);
        t
    }

    pub fn pcs(&self) -> Vec<u8> {
        self.tones().iter().map(|t| (self.root + t) % 12).collect()
    }

    pub fn mask(&self) -> u16 {
        self.pcs().iter().fold(0, |m, pc| m | 1 << pc)
    }

    pub fn name(&self) -> String {
        format!("{}{}", Key(self.root).name(), suffix(self.quality))
    }

    /// The pitch class of the third (or its replacement in sus chords).
    pub fn third_pc(&self) -> u8 {
        (self.root + quality_intervals(self.quality)[1]) % 12
    }

    pub fn fifth_pc(&self) -> u8 {
        (self.root + quality_intervals(self.quality)[2]) % 12
    }

    pub fn seventh_pc(&self) -> Option<u8> {
        quality_intervals(self.quality).get(3).filter(|i| **i < 12).map(|i| (self.root + i) % 12)
    }

    /// The tones to play with at most `n` voices, most important first: the
    /// root, the third, the seventh, extensions and last the fifth (kept
    /// earlier when it defines the chord: diminished and augmented).
    pub fn voice_tones(&self, n: usize) -> Vec<u8> {
        let core = quality_intervals(self.quality);
        let mut order: Vec<u8> = vec![core[0], core[1]];
        let fifth_defines = matches!(self.quality, Quality::Dim | Quality::Aug | Quality::Dim7 | Quality::M7b5);
        if fifth_defines {
            order.push(core[2]);
        }
        if let Some(&s) = core.get(3) {
            order.push(s);
        }
        order.extend(core.iter().skip(4));
        order.extend(&self.ext);
        if !fifth_defines {
            order.push(core[2]);
        }
        order.dedup();
        order.truncate(n.max(1));
        order.sort_by_key(|t| t % 12);
        order.iter().map(|t| (self.root + t) % 12).collect()
    }
}

/// The chord a spec entry names, in the parent scale.
pub fn resolve(spec: &ChordSpec, parent: Parent) -> ChordSym {
    let root = (parent.root as i32 + parent.degree_iv(spec.root.min(6) as i32) + spec.alter.clamp(-1, 1)).rem_euclid(12) as u8;
    ChordSym::new(root, spec.quality, resolve_ext(spec.quality, &spec.ext))
}

/// The spec entry for a detected chord (the reverse of [`resolve`]).
pub fn to_spec(sym: &ChordSym, parent: Parent, bar: u32, beat: f32, beats: f32) -> ChordSpec {
    let (root, alter) = parent.degree_of(sym.root);
    ChordSpec { bar, beat, beats, root: root as u32, alter, quality: sym.quality, ext: Vec::new() }
}

/// How rich a diatonic chord is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Triad,
    Seventh,
    Ninth,
}

/// The chord that stacking thirds on `degree` gives inside the parent scale,
/// at the requested richness. Extensions are words for the spec.
pub fn diatonic(parent: Parent, degree: i32, level: Level) -> (Quality, Vec<String>) {
    let at = |k: i32| parent.degree_iv(degree + k) - parent.degree_iv(degree);
    let (third, fifth, seventh) = (at(2), at(4), at(6));
    let triad = match (third, fifth) {
        (3, 7) => Quality::Min,
        (4, 7) => Quality::Maj,
        (3, 6) => Quality::Dim,
        (4, 8) => Quality::Aug,
        (3, _) => Quality::Min,
        _ => Quality::Maj,
    };
    if level == Level::Triad {
        return (triad, Vec::new());
    }
    let q7 = match (triad, seventh) {
        (Quality::Min, 10) => Quality::Min7,
        (Quality::Min, _) => Quality::Min7,
        (Quality::Maj, 11) => Quality::Maj7,
        (Quality::Maj, _) => Quality::Dom7,
        (Quality::Dim, 10) => Quality::M7b5,
        (Quality::Dim, _) => Quality::Dim7,
        (Quality::Aug, _) => Quality::Maj7,
        _ => Quality::Maj7,
    };
    if level == Level::Seventh {
        return (q7, Vec::new());
    }
    // A major ninth only: otherwise the seventh chord stands.
    let ninth = at(8);
    let q9 = match q7 {
        Quality::Min7 if ninth == 14 => Quality::Min9,
        Quality::Maj7 if ninth == 14 => Quality::Maj9,
        Quality::Dom7 if ninth == 14 => Quality::Dom9,
        other => other,
    };
    (q9, Vec::new())
}

/// The chord a set of pitch classes most looks like, if any. `bass` is the
/// lowest pitch class and wins ties.
pub fn identify(mask: u16, bass: Option<u8>) -> Option<ChordSym> {
    use Quality::*;
    const CANDIDATES: [Quality; 12] = [Min7, Maj7, Dom7, Min, Maj, Min9, Dom9, Maj9, Dim, M7b5, Sus4, Sus2];
    if mask.count_ones() < 3 {
        return None;
    }
    let mut best: Option<(i32, ChordSym)> = None;
    for root in 0..12u8 {
        for q in CANDIDATES {
            let tm = quality_intervals(q).iter().fold(0u16, |m, i| m | 1 << ((root + i) % 12));
            let hit = (tm & mask).count_ones() as i32;
            let extra = (mask & !tm).count_ones() as i32;
            let missing = (tm & !mask).count_ones() as i32;
            if hit < 3 || missing > 1 {
                continue;
            }
            // Root and third (or its replacement) must be there.
            let third = quality_intervals(q)[1];
            if mask & (1 << root) == 0 || mask & (1 << ((root + third) % 12)) == 0 {
                continue;
            }
            let mut score = hit * 4 - extra * 3 - missing * 5 - quality_intervals(q).len() as i32 / 2;
            if bass == Some(root) {
                score += 3;
            }
            if best.as_ref().is_none_or(|(s, _)| score > *s) {
                best = Some((score, ChordSym::new(root, q, Vec::new())));
            }
        }
    }
    best.map(|(_, c)| c)
}

// ───────────────────────────── Voicing ─────────────────────────────

/// How chords are voiced.
#[derive(Clone, Copy, Debug)]
pub struct VoiceStyle {
    /// Most voices per chord.
    pub voices: usize,
    /// Close to each other (house keys) or open (pads).
    pub compact: bool,
}

fn next_above(pc: u8, floor: i32) -> i32 {
    let mut n = floor + 1;
    while (n.rem_euclid(12)) as u8 != pc % 12 {
        n += 1;
    }
    n
}

/// Every voicing worth considering for a chord: closed positions in every
/// inversion and octave, and their drop-2 versions; within the chord range,
/// no gap above an octave between neighbours.
pub fn candidates(sym: &ChordSym, style: VoiceStyle) -> Vec<Vec<u8>> {
    let pcs = sym.voice_tones(style.voices);
    let m = pcs.len();
    let mut out: Vec<Vec<u8>> = Vec::new();
    for k in 0..m {
        for low in (CHORD_BOTTOM as i32)..=(CHORD_TOP as i32) {
            if (low.rem_euclid(12)) as u8 != pcs[k] {
                continue;
            }
            let mut v = vec![low];
            for j in 1..m {
                v.push(next_above(pcs[(k + j) % m], *v.last().unwrap_or(&low)));
            }
            let mut push = |mut c: Vec<i32>| {
                c.sort_unstable();
                let ok = c.first().is_some_and(|b| *b >= CHORD_BOTTOM as i32) && c.last().is_some_and(|t| *t <= CHORD_TOP as i32) && c.windows(2).all(|w| w[1] - w[0] <= 12 && w[1] > w[0]);
                if ok {
                    let notes: Vec<u8> = c.iter().map(|n| *n as u8).collect();
                    if !out.contains(&notes) {
                        out.push(notes);
                    }
                }
            };
            push(v.clone());
            if m >= 3 {
                let mut d = v.clone();
                let i = d.len() - 2;
                d[i] -= 12;
                push(d);
            }
        }
    }
    out
}

fn centre(v: &[u8]) -> f32 {
    v.iter().map(|n| *n as f32).sum::<f32>() / v.len().max(1) as f32
}

fn style_cost(v: &[u8], style: VoiceStyle) -> f32 {
    let span = (v[v.len() - 1] - v[0]) as f32;
    let target = if style.compact { 10. } else { 15. };
    // A semitone between neighbours is a cluster: the ninth should sit apart from the third.
    let clusters = v.windows(2).filter(|w| w[1] - w[0] == 1).count() as f32;
    (centre(v) - CHORD_CENTRE).abs() / 12. + 0.5 * (span - target).abs() / 4. + 4. * clusters
}

/// Cost of moving from voicing `a` to `b`. Voices are paired from the top
/// down; the upper voice is the one that is heard as a line.
pub fn move_cost(a: &[u8], b: &[u8], style: VoiceStyle) -> f32 {
    let n = a.len().min(b.len());
    let pa = &a[a.len() - n..];
    let pb = &b[b.len() - n..];
    let mut cost = 0.;
    let mut common = 0;
    for i in 0..n {
        let d = (pb[i] as i32 - pa[i] as i32).abs();
        cost += d as f32;
        if d == 0 {
            common += 1;
        }
    }
    cost += 2. * (a.len() as i32 - b.len() as i32).unsigned_abs() as f32;
    let top = (b[b.len() - 1] as i32 - a[a.len() - 1] as i32).abs();
    cost += 3. * (top - 2).max(0) as f32;
    if top > 7 {
        cost += 20.;
    }
    cost -= 2. * common as f32;
    if a.len() == b.len() {
        for i in 0..n {
            for j in i + 1..n {
                if parallel(pa[i], pa[j], pb[i], pb[j]) {
                    cost += if i == 0 && j == n - 1 { 12. } else { 4. };
                }
            }
        }
    }
    cost + style_cost(b, style)
}

/// Perfect fifths or octaves moving together in the same direction.
pub fn parallel(a_lo: u8, a_hi: u8, b_lo: u8, b_hi: u8) -> bool {
    let ia = (a_hi as i32 - a_lo as i32).rem_euclid(12);
    let ib = (b_hi as i32 - b_lo as i32).rem_euclid(12);
    let (m_lo, m_hi) = (b_lo as i32 - a_lo as i32, b_hi as i32 - a_hi as i32);
    (ia == 0 || ia == 7) && ia == ib && m_lo != 0 && m_hi != 0 && m_lo.signum() == m_hi.signum()
}

/// A voicing per chord that minimises the total cost (Viterbi over about 30
/// candidates per chord). `seed` only breaks ties between near-equal
/// voicings, so a new seed varies the result without making it worse.
pub fn voice_lead(chords: &[ChordSym], style: VoiceStyle, seed: u64) -> Vec<Vec<u8>> {
    let mut rng = Rng::new(seed);
    let cands: Vec<Vec<(Vec<u8>, f32)>> = chords
        .iter()
        .map(|c| {
            let mut v = candidates(c, style);
            if v.is_empty() {
                // Always something playable: the closed chord from the root, folded into range.
                v.push(fallback(c));
            }
            v.into_iter().map(|x| (x, rng.between(0., 0.3))).collect()
        })
        .collect();
    if cands.is_empty() {
        return Vec::new();
    }
    let mut best: Vec<f32> = cands[0].iter().map(|(v, j)| style_cost(v, style) + j).collect();
    let mut back: Vec<Vec<usize>> = vec![vec![0; cands[0].len()]];
    for i in 1..cands.len() {
        let mut next = Vec::with_capacity(cands[i].len());
        let mut from = Vec::with_capacity(cands[i].len());
        for (v, j) in &cands[i] {
            let (arg, c) = cands[i - 1]
                .iter()
                .enumerate()
                .map(|(p, (pv, _))| (p, best[p] + move_cost(pv, v, style)))
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .unwrap_or((0, 0.));
            next.push(c + j);
            from.push(arg);
        }
        best = next;
        back.push(from);
    }
    let mut idx = best.iter().enumerate().min_by(|a, b| a.1.total_cmp(b.1)).map_or(0, |(i, _)| i);
    let mut out = vec![Vec::new(); cands.len()];
    for i in (0..cands.len()).rev() {
        out[i] = cands[i][idx].0.clone();
        idx = back[i][idx];
    }
    out
}

fn fallback(c: &ChordSym) -> Vec<u8> {
    let mut last = 52i32;
    let mut v = Vec::new();
    for pc in c.voice_tones(4) {
        last = next_above(pc, last);
        v.push(last.clamp(CHORD_BOTTOM as i32, CHORD_TOP as i32) as u8);
    }
    v.sort_unstable();
    v.dedup();
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitrack_model::Key;

    fn a_minor() -> Parent {
        Parent::of(Scale::new(Key(9), ScaleKind::Minor))
    }

    fn spec(root: u32, alter: i32, q: Quality, ext: &[&str]) -> ChordSpec {
        ChordSpec { root, alter, quality: q, ext: ext.iter().map(|s| s.to_string()).collect(), ..Default::default() }
    }

    #[test]
    fn the_deep_house_loop_resolves_to_am7_dmaj7_e9_am7() {
        let p = a_minor();
        let names: Vec<_> = [spec(0, 0, Quality::Min7, &[]), spec(3, 0, Quality::Maj7, &[]), spec(4, 0, Quality::Dom9, &[]), spec(0, 0, Quality::Min7, &[])].iter().map(|c| resolve(c, p).name()).collect();
        assert_eq!(names, ["Am7", "Dmaj7", "E9", "Am7"]);
        // bII in phrygian territory: Bb major from the second degree lowered.
        assert_eq!(resolve(&spec(1, -1, Quality::Maj7, &[]), p).name(), "A#maj7");
    }

    #[test]
    fn pentatonic_scales_stack_on_the_natural_minor_or_major() {
        let m = Parent::of(Scale::new(Key(9), ScaleKind::MinorPentatonic));
        assert_eq!((m.kind, m.root), (ScaleKind::Minor, 9));
        assert_eq!(Parent::of(Scale::new(Key(0), ScaleKind::Blues)).kind, ScaleKind::Minor);
        assert_eq!(Parent::of(Scale::new(Key(0), ScaleKind::MajorPentatonic)).kind, ScaleKind::Major);
        assert_eq!(Parent::of(Scale::new(Key(2), ScaleKind::Chromatic)).kind, ScaleKind::Minor);
    }

    #[test]
    fn diatonic_chords_follow_the_scale() {
        let p = a_minor();
        assert_eq!(diatonic(p, 0, Level::Seventh).0, Quality::Min7);
        assert_eq!(diatonic(p, 2, Level::Seventh).0, Quality::Maj7);
        assert_eq!(diatonic(p, 6, Level::Seventh).0, Quality::Dom7);
        assert_eq!(diatonic(p, 1, Level::Seventh).0, Quality::M7b5);
        assert_eq!(diatonic(p, 0, Level::Ninth).0, Quality::Min9);
        assert_eq!(diatonic(p, 5, Level::Triad).0, Quality::Maj);
        let harmonic = Parent::of(Scale::new(Key(9), ScaleKind::HarmonicMinor));
        assert_eq!(diatonic(harmonic, 4, Level::Seventh).0, Quality::Dom7, "the V of harmonic minor");
        let dorian = Parent::of(Scale::new(Key(2), ScaleKind::Dorian));
        assert_eq!(diatonic(dorian, 3, Level::Seventh).0, Quality::Dom7);
    }

    #[test]
    fn clashing_extensions_are_dropped() {
        let w = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert!(resolve_ext(Quality::Maj7, &w(&["11"])).is_empty(), "natural 11 over a major third");
        assert_eq!(resolve_ext(Quality::Min7, &w(&["9", "11"])), vec![14, 17]);
        assert!(resolve_ext(Quality::Min7, &w(&["b9"])).is_empty(), "flat 9 only on dominants");
        assert_eq!(resolve_ext(Quality::Dom7, &w(&["b9"])), vec![13]);
        assert!(resolve_ext(Quality::Min9, &w(&["9"])).is_empty(), "already there");
        assert_eq!(resolve_ext(Quality::Maj7, &w(&["#11"])), vec![18]);
    }

    #[test]
    fn identify_finds_common_chords() {
        let mask = |pcs: &[u8]| pcs.iter().fold(0u16, |m, p| m | 1 << p);
        assert_eq!(identify(mask(&[9, 0, 4, 7]), Some(9)).unwrap().name(), "Am7");
        assert_eq!(identify(mask(&[2, 6, 9, 1]), Some(2)).unwrap().name(), "Dmaj7");
        assert_eq!(identify(mask(&[7, 11, 2]), Some(7)).unwrap().name(), "G");
        assert!(identify(mask(&[0, 7]), None).is_none());
        assert!(identify(0, None).is_none());
    }

    fn style() -> VoiceStyle {
        VoiceStyle { voices: 4, compact: true }
    }

    #[test]
    fn candidates_are_in_range_and_compact() {
        let sym = resolve(&spec(0, 0, Quality::Min9, &[]), a_minor());
        let c = candidates(&sym, style());
        assert!(c.len() >= 8, "{}", c.len());
        for v in &c {
            assert!(v[0] >= CHORD_BOTTOM && *v.last().unwrap() <= CHORD_TOP, "{v:?}");
            assert!(v.windows(2).all(|w| w[1] - w[0] <= 12));
        }
        // The seventh survives a four-voice voicing of a ninth chord; the fifth goes first.
        let pcs: Vec<u8> = sym.voice_tones(4);
        assert!(pcs.contains(&sym.seventh_pc().unwrap()) && !pcs.contains(&sym.fifth_pc()), "{pcs:?}");
    }

    #[test]
    fn the_loop_is_voiced_smoothly() {
        let p = a_minor();
        let chords: Vec<_> = [spec(0, 0, Quality::Min7, &[]), spec(3, 0, Quality::Maj7, &[]), spec(4, 0, Quality::Dom9, &[]), spec(0, 0, Quality::Min7, &[])].iter().map(|c| resolve(c, p)).collect();
        let v = voice_lead(&chords, style(), 3);
        assert_eq!(v.len(), 4);
        for (i, w) in v.windows(2).enumerate() {
            let jump = (*w[1].last().unwrap() as i32 - *w[0].last().unwrap() as i32).abs();
            assert!(jump <= 5, "transition {i}: top voice jumps {jump}: {v:?}");
        }
        for (voicing, c) in v.iter().zip(&chords) {
            let pcs: Vec<u8> = voicing.iter().map(|n| n % 12).collect();
            assert!(pcs.iter().all(|pc| c.pcs().contains(pc)), "{voicing:?} for {}", c.name());
            assert!(pcs.contains(&c.third_pc()) && pcs.contains(&c.seventh_pc().unwrap()), "guide tones kept in {}", c.name());
        }
    }

    #[test]
    fn common_tones_stay_in_the_same_voice() {
        // Dm7 to G7 share D and F... through the pair Am7 -> Cmaj7 (C, E, G shared).
        let p = a_minor();
        let a = resolve(&spec(0, 0, Quality::Min7, &[]), p);
        let c = resolve(&spec(2, 0, Quality::Maj7, &[]), p);
        let v = voice_lead(&[a, c], style(), 1);
        let kept = v[0].iter().zip(&v[1]).filter(|(x, y)| x == y).count();
        assert!(kept >= 1, "{v:?}");
    }

    #[test]
    fn a_new_seed_can_change_the_voicing_but_not_its_quality() {
        let p = a_minor();
        let chords: Vec<_> = [spec(0, 0, Quality::Min9, &[]), spec(5, 0, Quality::Maj7, &[])].iter().map(|c| resolve(c, p)).collect();
        let costs: Vec<f32> = (0..20u64).map(|s| voice_lead(&chords, style(), s).windows(2).map(|w| move_cost(&w[0], &w[1], style())).sum()).collect();
        let (lo, hi) = costs.iter().fold((f32::MAX, f32::MIN), |(l, h), c| (l.min(*c), h.max(*c)));
        assert!(hi - lo < 1.0, "{lo} {hi}");
        assert_eq!(voice_lead(&chords, style(), 5), voice_lead(&chords, style(), 5));
    }

    #[test]
    fn parallel_fifths_between_outer_voices_are_avoided() {
        assert!(parallel(48, 55, 50, 57));
        assert!(!parallel(48, 55, 48, 57));
        assert!(!parallel(48, 55, 50, 58));
    }
}
