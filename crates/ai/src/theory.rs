//! Rule-based music theory: diatonic chord progressions, basslines, arpeggios
//! and genre drum patterns, all returned as project [`Note`]s. Used by the AI
//! features and the MCP tools (`suggest_chords`, `suggest_bassline`...).
//!
//! Note ids are local (1..=n); callers allocate real ids when inserting.

use aitrack_model::{Note, NoteId, PPQ, Scale, ScaleKind, Tick, TimeSignature};
use serde::{Deserialize, Serialize};

/// A chord as a scale degree (0-based: 0 = tonic) built by stacking thirds
/// inside the scale, so `degree 5` in A minor is F major.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Degree {
    pub degree: i32,
    /// 3 = triad, 4 = seventh chord, 5 = ninth.
    pub size: u8,
}

impl Degree {
    pub fn triad(degree: i32) -> Self {
        Self { degree, size: 3 }
    }

    /// Scale-degree offsets of the chord tones (0, 2, 4, 6...).
    fn steps(self) -> impl Iterator<Item = i32> {
        (0..self.size as i32).map(move |k| self.degree + 2 * k)
    }
}

/// Parse "i-VI-III-VII", "I V vi IV", "ii7-V7-I". Case only documents the
/// expected quality; the actual chord is diatonic to the scale.
pub fn parse_roman(text: &str) -> Option<Vec<Degree>> {
    let mut out = Vec::new();
    for tok in text.split(['-', ' ', ',', '|']).filter(|t| !t.is_empty()) {
        let t = tok.trim();
        let size = if t.ends_with('9') {
            5
        } else if t.ends_with('7') {
            4
        } else {
            3
        };
        let core: String = t
            .chars()
            .filter(|c| c.is_ascii_alphabetic())
            .collect::<String>()
            .to_ascii_lowercase();
        let core = core
            .trim_end_matches("dim")
            .trim_end_matches("maj")
            .trim_end_matches('m');
        let degree = match core {
            "i" => 0,
            "ii" => 1,
            "iii" => 2,
            "iv" => 3,
            "v" => 4,
            "vi" => 5,
            "vii" => 6,
            _ => return None,
        };
        out.push(Degree { degree, size });
    }
    (!out.is_empty()).then_some(out)
}

/// Common progressions by genre.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressionPreset {
    /// i–VI–III–VII: EDM / reggaeton / pop in minor.
    EpicMinor,
    /// i–VII–VI–V (Andalusian cadence; V is the scale's v, so use harmonic minor for a major V).
    Andalusian,
    /// I–V–vi–IV.
    Pop,
    /// vi–IV–I–V ("sensitive female" progression).
    PopMinor,
    /// i–iv–v–i.
    MinorBlues,
    /// ii–V–I–I.
    TwoFiveOne,
    /// i–VI–VII–i: dark trap / drill.
    Trap,
    /// i–iv–VI–V (reggaeton classic).
    Reggaeton,
}

impl ProgressionPreset {
    pub fn degrees(self) -> Vec<Degree> {
        let d = |v: &[i32]| v.iter().map(|&x| Degree::triad(x)).collect();
        match self {
            Self::EpicMinor => d(&[0, 5, 2, 6]),
            Self::Andalusian => d(&[0, 6, 5, 4]),
            Self::Pop => d(&[0, 4, 5, 3]),
            Self::PopMinor => d(&[5, 3, 0, 4]),
            Self::MinorBlues => d(&[0, 3, 4, 0]),
            Self::TwoFiveOne => vec![
                Degree { degree: 1, size: 4 },
                Degree { degree: 4, size: 4 },
                Degree { degree: 0, size: 4 },
                Degree { degree: 0, size: 4 },
            ],
            Self::Trap => d(&[0, 5, 6, 0]),
            Self::Reggaeton => d(&[0, 3, 5, 4]),
        }
    }
}

/// Common parameters.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Timing {
    pub signature: TimeSignature,
    /// Bars per chord.
    pub bars_per_chord: u32,
    /// Total bars to fill (the progression repeats).
    pub bars: u32,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            signature: TimeSignature::default(),
            bars_per_chord: 1,
            bars: 4,
        }
    }
}

struct Builder {
    notes: Vec<Note>,
}

impl Builder {
    fn new() -> Self {
        Self { notes: Vec::new() }
    }
    fn add(&mut self, key: i32, start: Tick, length: Tick, velocity: f32) {
        if (0..=127).contains(&key) && length > 0 {
            self.notes
                .push(Note::new(NoteId(0), key as u8, start, length, velocity));
        }
    }
    fn finish(mut self) -> Vec<Note> {
        self.notes
            .sort_by(|a, b| a.start.cmp(&b.start).then(a.key.cmp(&b.key)));
        for (i, n) in self.notes.iter_mut().enumerate() {
            n.id = NoteId(i as u64 + 1);
        }
        self.notes
    }
}

/// Chord tones as MIDI notes with simple voice leading: each chord is
/// inverted to stay close to the previous one around `octave` (4 = C4 region).
pub fn chord_notes(scale: Scale, chord: Degree, octave: i32) -> Vec<i32> {
    chord
        .steps()
        .map(|s| scale.degree_to_note(s, octave) as i32)
        .collect()
}

fn voice_lead(prev: Option<&[i32]>, mut chord: Vec<i32>) -> Vec<i32> {
    let Some(prev) = prev else { return chord };
    let center = prev.iter().sum::<i32>() as f32 / prev.len() as f32;
    let mut best = chord.clone();
    let mut best_d = f32::MAX;
    for _ in 0..chord.len() * 2 {
        let c = chord.iter().sum::<i32>() as f32 / chord.len() as f32;
        let d = (c - center).abs();
        if d < best_d {
            best_d = d;
            best = chord.clone();
        }
        // Next inversion upward.
        chord.sort();
        let low = chord.remove(0);
        chord.push(low + 12);
    }
    // Try one inversion downward as well.
    let mut down = best.clone();
    down.sort();
    if let Some(high) = down.pop() {
        down.insert(0, high - 12);
        let c = down.iter().sum::<i32>() as f32 / down.len() as f32;
        if (c - center).abs() < best_d {
            best = down;
        }
    }
    best
}

/// Block chords for a progression.
pub fn progression(scale: Scale, chords: &[Degree], octave: i32, timing: Timing) -> Vec<Note> {
    let mut b = Builder::new();
    let bar = timing.signature.bar_ticks();
    let span = bar * timing.bars_per_chord.max(1) as Tick;
    let mut prev: Option<Vec<i32>> = None;
    let mut t = 0;
    let mut i = 0;
    while t < bar * timing.bars as Tick && !chords.is_empty() {
        let voiced = voice_lead(
            prev.as_deref(),
            chord_notes(scale, chords[i % chords.len()], octave),
        );
        for &k in &voiced {
            b.add(k, t, span, 0.75);
        }
        prev = Some(voiced);
        t += span;
        i += 1;
    }
    b.finish()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BassStyle {
    /// Whole-chord root notes.
    Roots,
    /// Eighth-note roots alternating with the octave.
    Octaves,
    /// Off-beat eighths (EDM / house "pumping" bass).
    Offbeat,
    /// Reggaeton dembow bass: hits on 1, the "and" of 2 (3+3+2 feel).
    Dembow,
    /// Long 808 notes on the kick pattern with a slide into the next chord.
    Trap808,
}

/// Bassline following the chord roots.
pub fn bassline(
    scale: Scale,
    chords: &[Degree],
    octave: i32,
    style: BassStyle,
    timing: Timing,
) -> Vec<Note> {
    let mut b = Builder::new();
    let bar = timing.signature.bar_ticks();
    let span = bar * timing.bars_per_chord.max(1) as Tick;
    let e = PPQ / 2;
    let s16 = PPQ / 4;
    let total = bar * timing.bars as Tick;
    let mut t = 0;
    let mut i = 0;
    while t < total && !chords.is_empty() {
        let root = scale.degree_to_note(chords[i % chords.len()].degree, octave) as i32;
        let fifth = scale.degree_to_note(chords[i % chords.len()].degree + 4, octave) as i32;
        match style {
            BassStyle::Roots => b.add(root, t, span, 0.8),
            BassStyle::Octaves => {
                let mut x = 0;
                while x < span {
                    b.add(
                        if (x / e) % 2 == 0 { root } else { root + 12 },
                        t + x,
                        e - 20,
                        0.8,
                    );
                    x += e;
                }
            }
            BassStyle::Offbeat => {
                let mut x = e;
                while x < span {
                    b.add(root, t + x, e - 40, 0.85);
                    x += PPQ;
                }
            }
            BassStyle::Dembow => {
                let mut bar_t = 0;
                while bar_t < span {
                    // 16th steps 0, 3 (anticipation) and 8, 11, with the 5th on 11.
                    for (step, k, len) in [(0, root, 3), (3, root, 3), (8, root, 3), (11, fifth, 3)]
                    {
                        b.add(
                            k,
                            t + bar_t + step * s16,
                            len * s16 - 20,
                            if step == 0 { 0.9 } else { 0.75 },
                        );
                    }
                    bar_t += bar;
                }
            }
            BassStyle::Trap808 => {
                let mut bar_t = 0;
                while bar_t < span {
                    b.add(root, t + bar_t, 6 * s16, 0.95);
                    b.add(root, t + bar_t + 10 * s16, 6 * s16, 0.85);
                    bar_t += bar;
                }
                if let Some(n) = b.notes.last_mut() {
                    n.slide = true;
                }
            }
        }
        t += span;
        i += 1;
    }
    b.finish()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArpPattern {
    Up,
    Down,
    UpDown,
    /// Deterministic shuffle from a seed.
    Random(u32),
}

/// Arpeggiate each chord at `rate` ticks per step over `octaves` octaves.
pub fn arp(
    scale: Scale,
    chords: &[Degree],
    octave: i32,
    pattern: ArpPattern,
    rate: Tick,
    octaves: u32,
    timing: Timing,
) -> Vec<Note> {
    let mut b = Builder::new();
    let bar = timing.signature.bar_ticks();
    let span = bar * timing.bars_per_chord.max(1) as Tick;
    let rate = rate.max(1);
    let total = bar * timing.bars as Tick;
    let mut seed = match pattern {
        ArpPattern::Random(s) => s.wrapping_mul(2_654_435_761).max(1),
        _ => 1,
    };
    let mut t = 0;
    let mut i = 0;
    while t < total && !chords.is_empty() {
        let base = chord_notes(scale, chords[i % chords.len()], octave);
        let mut tones: Vec<i32> = (0..octaves.max(1) as i32)
            .flat_map(|o| base.iter().map(move |k| k + 12 * o))
            .collect();
        tones.sort();
        let seq: Vec<i32> = match pattern {
            ArpPattern::Up | ArpPattern::Random(_) => tones.clone(),
            ArpPattern::Down => tones.iter().rev().copied().collect(),
            ArpPattern::UpDown => {
                let mut v = tones.clone();
                v.extend(
                    tones
                        .iter()
                        .rev()
                        .skip(1)
                        .take(tones.len().saturating_sub(2)),
                );
                v
            }
        };
        let mut x = 0;
        let mut k = 0usize;
        while x < span {
            let key = if let ArpPattern::Random(_) = pattern {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                tones[seed as usize % tones.len()]
            } else {
                seq[k % seq.len()]
            };
            b.add(
                key,
                t + x,
                rate - rate / 8,
                if k.is_multiple_of(4) { 0.85 } else { 0.7 },
            );
            x += rate;
            k += 1;
        }
        t += span;
        i += 1;
    }
    b.finish()
}

/// General MIDI drum keys used by [`drum_pattern`].
pub mod gm {
    pub const KICK: u8 = 36;
    pub const RIM: u8 = 37;
    pub const SNARE: u8 = 38;
    pub const CLAP: u8 = 39;
    pub const CLOSED_HAT: u8 = 42;
    pub const OPEN_HAT: u8 = 46;
    pub const CRASH: u8 = 49;
    pub const SHAKER: u8 = 70;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DrumStyle {
    /// Reggaeton dembow: four-on-the-floor kick, snare on 16ths 3, 6, 11, 14.
    Dembow,
    /// EDM / house: kick every beat, clap on 2 and 4, open hat off-beats, 16th closed hats.
    FourOnTheFloor,
    /// Trap: half-time snare on 3, sparse kicks, 16th hats with a triplet roll.
    Trap,
    /// Breakbeat (two-step): kick 1 and the "and" of 3, snare 2 and 4.
    Breakbeat,
}

/// A drum pattern for `bars` bars of 4/4 (16 steps per bar).
pub fn drum_pattern(style: DrumStyle, bars: u32) -> Vec<Note> {
    use gm::*;
    let s = PPQ / 4;
    let bar = 16 * s;
    let mut b = Builder::new();
    let hit = |b: &mut Builder, bar_i: u32, step: Tick, key: u8, vel: f32| {
        b.add(key as i32, bar_i as Tick * bar + step * s, s / 2, vel)
    };
    for i in 0..bars {
        match style {
            DrumStyle::Dembow => {
                for st in [0, 4, 8, 12] {
                    hit(&mut b, i, st, KICK, 0.95);
                }
                for st in [3, 6, 11, 14] {
                    hit(&mut b, i, st, SNARE, 0.85);
                }
                for st in (0..16).step_by(2) {
                    hit(
                        &mut b,
                        i,
                        st,
                        CLOSED_HAT,
                        if st % 4 == 0 { 0.6 } else { 0.5 },
                    );
                }
            }
            DrumStyle::FourOnTheFloor => {
                for st in [0, 4, 8, 12] {
                    hit(&mut b, i, st, KICK, 1.0);
                }
                for st in [4, 12] {
                    hit(&mut b, i, st, CLAP, 0.9);
                }
                for st in [2, 6, 10, 14] {
                    hit(&mut b, i, st, OPEN_HAT, 0.7);
                }
                for st in [1, 3, 5, 7, 9, 11, 13, 15] {
                    hit(&mut b, i, st, CLOSED_HAT, 0.45);
                }
                if i == 0 {
                    hit(&mut b, i, 0, CRASH, 0.8);
                }
            }
            DrumStyle::Trap => {
                for st in [0, 7, 10] {
                    hit(&mut b, i, st, KICK, 0.95);
                }
                hit(&mut b, i, 8, SNARE, 0.95);
                hit(&mut b, i, 8, CLAP, 0.8);
                for st in 0..16 {
                    if st == 14 && i % 2 == 1 {
                        // Hat roll: 1/32 triplets over the last eighth.
                        let roll = PPQ / 6;
                        for r in 0..3 {
                            b.add(
                                CLOSED_HAT as i32,
                                i as Tick * bar + 14 * s + r * roll,
                                roll / 2,
                                0.55,
                            );
                        }
                        continue;
                    }
                    if st == 15 && i % 2 == 1 {
                        continue;
                    }
                    hit(
                        &mut b,
                        i,
                        st,
                        CLOSED_HAT,
                        if st % 2 == 0 { 0.6 } else { 0.4 },
                    );
                }
            }
            DrumStyle::Breakbeat => {
                for st in [0, 10] {
                    hit(&mut b, i, st, KICK, 0.95);
                }
                for st in [4, 12] {
                    hit(&mut b, i, st, SNARE, 0.9);
                }
                for st in (0..16).step_by(2) {
                    hit(&mut b, i, st, CLOSED_HAT, 0.5);
                }
                hit(&mut b, i, 7, SHAKER, 0.4);
            }
        }
    }
    b.finish()
}

/// Every note of `notes` belongs to `scale` (drums excluded).
pub fn all_in_scale(notes: &[Note], scale: Scale) -> bool {
    scale.kind == ScaleKind::Chromatic || notes.iter().all(|n| scale.contains(n.key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aitrack_model::Key;

    fn am() -> Scale {
        Scale::new(Key(9), ScaleKind::Minor)
    }

    #[test]
    fn roman_parsing() {
        let d = parse_roman("i-VI-III-VII").unwrap();
        assert_eq!(
            d.iter().map(|x| x.degree).collect::<Vec<_>>(),
            vec![0, 5, 2, 6]
        );
        let j = parse_roman("ii7 V7 Imaj7").unwrap();
        assert_eq!(
            j.iter().map(|x| (x.degree, x.size)).collect::<Vec<_>>(),
            vec![(1, 4), (4, 4), (0, 4)]
        );
        assert!(parse_roman("x-y").is_none());
    }

    #[test]
    fn epic_minor_in_a_minor_is_am_f_c_g() {
        let notes = progression(
            am(),
            &ProgressionPreset::EpicMinor.degrees(),
            4,
            Timing::default(),
        );
        assert_eq!(notes.len(), 12);
        assert!(all_in_scale(&notes, am()));
        let bar = PPQ * 4;
        let pcs = |i: i64| {
            let mut v: Vec<u8> = notes
                .iter()
                .filter(|n| n.start == i * bar)
                .map(|n| n.key % 12)
                .collect();
            v.sort();
            v
        };
        assert_eq!(pcs(0), vec![0, 4, 9]); // A C E
        assert_eq!(pcs(1), vec![0, 5, 9]); // F A C
        assert_eq!(pcs(2), vec![0, 4, 7]); // C E G
        assert_eq!(pcs(3), vec![2, 7, 11]); // G B D
        // Voice leading keeps chords within an octave-ish of each other.
        let span =
            notes.iter().map(|n| n.key).max().unwrap() - notes.iter().map(|n| n.key).min().unwrap();
        assert!(span <= 14, "span {span}");
    }

    #[test]
    fn basslines_follow_roots() {
        let deg = ProgressionPreset::EpicMinor.degrees();
        let roots = bassline(am(), &deg, 2, BassStyle::Roots, Timing::default());
        assert_eq!(
            roots.iter().map(|n| n.key).collect::<Vec<_>>(),
            vec![45, 53, 48, 55]
        );
        let dembow = bassline(am(), &deg, 2, BassStyle::Dembow, Timing::default());
        assert_eq!(dembow.len(), 16);
        assert!(all_in_scale(&dembow, am()));
        let off = bassline(am(), &deg, 2, BassStyle::Offbeat, Timing::default());
        assert!(off.iter().all(|n| n.start % PPQ == PPQ / 2));
    }

    #[test]
    fn arps_stay_in_scale() {
        let deg = ProgressionPreset::Pop.degrees();
        let c = Scale::new(Key(0), ScaleKind::Major);
        let up = arp(c, &deg, 4, ArpPattern::Up, PPQ / 4, 2, Timing::default());
        assert_eq!(up.len(), 64);
        assert!(all_in_scale(&up, c));
        assert_eq!(
            &up[..3].iter().map(|n| n.key).collect::<Vec<_>>(),
            &[60, 64, 67]
        );
        let rnd = arp(
            c,
            &deg,
            4,
            ArpPattern::Random(7),
            PPQ / 4,
            1,
            Timing::default(),
        );
        assert!(all_in_scale(&rnd, c));
    }

    #[test]
    fn dembow_pattern() {
        let p = drum_pattern(DrumStyle::Dembow, 2);
        let s = PPQ / 4;
        let snares: Vec<i64> = p
            .iter()
            .filter(|n| n.key == gm::SNARE && n.start < 16 * s)
            .map(|n| n.start / s)
            .collect();
        assert_eq!(snares, vec![3, 6, 11, 14]);
        let kicks = p.iter().filter(|n| n.key == gm::KICK).count();
        assert_eq!(kicks, 8);
        let four = drum_pattern(DrumStyle::FourOnTheFloor, 1);
        assert_eq!(
            four.iter()
                .filter(|n| n.key == gm::KICK)
                .map(|n| n.start)
                .collect::<Vec<_>>(),
            vec![0, PPQ, 2 * PPQ, 3 * PPQ]
        );
    }
}
