//! Keys and scales: the project's global scale drives piano-roll
//! highlighting, MIDI tools, sample recommendations and AI generation.

use serde::{Deserialize, Serialize};

/// Pitch class 0..12 (C = 0).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Key(pub u8);

impl Key {
    pub const NAMES_SHARP: [&'static str; 12] =
        ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

    pub fn name(self) -> &'static str {
        Self::NAMES_SHARP[(self.0 % 12) as usize]
    }

    pub fn from_name(name: &str) -> Option<Self> {
        let n = name.trim();
        let (letter, rest) = n.split_at(n.char_indices().nth(1).map_or(n.len(), |(i, _)| i));
        let base = match letter.to_ascii_uppercase().as_str() {
            "C" => 0,
            "D" => 2,
            "E" => 4,
            "F" => 5,
            "G" => 7,
            "A" => 9,
            "B" => 11,
            _ => return None,
        };
        let shift: i32 = match rest {
            "" => 0,
            "#" | "♯" => 1,
            "b" | "♭" => -1,
            _ => return None,
        };
        Some(Key(((base + shift).rem_euclid(12)) as u8))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScaleKind {
    Major,
    Minor,
    HarmonicMinor,
    MelodicMinor,
    Dorian,
    Phrygian,
    Lydian,
    Mixolydian,
    Locrian,
    MajorPentatonic,
    MinorPentatonic,
    Blues,
    Chromatic,
}

impl ScaleKind {
    pub const ALL: [ScaleKind; 13] = [
        Self::Major,
        Self::Minor,
        Self::HarmonicMinor,
        Self::MelodicMinor,
        Self::Dorian,
        Self::Phrygian,
        Self::Lydian,
        Self::Mixolydian,
        Self::Locrian,
        Self::MajorPentatonic,
        Self::MinorPentatonic,
        Self::Blues,
        Self::Chromatic,
    ];

    /// Semitone offsets from the root.
    pub fn intervals(self) -> &'static [u8] {
        match self {
            Self::Major => &[0, 2, 4, 5, 7, 9, 11],
            Self::Minor => &[0, 2, 3, 5, 7, 8, 10],
            Self::HarmonicMinor => &[0, 2, 3, 5, 7, 8, 11],
            Self::MelodicMinor => &[0, 2, 3, 5, 7, 9, 11],
            Self::Dorian => &[0, 2, 3, 5, 7, 9, 10],
            Self::Phrygian => &[0, 1, 3, 5, 7, 8, 10],
            Self::Lydian => &[0, 2, 4, 6, 7, 9, 11],
            Self::Mixolydian => &[0, 2, 4, 5, 7, 9, 10],
            Self::Locrian => &[0, 1, 3, 5, 6, 8, 10],
            Self::MajorPentatonic => &[0, 2, 4, 7, 9],
            Self::MinorPentatonic => &[0, 3, 5, 7, 10],
            Self::Blues => &[0, 3, 5, 6, 7, 10],
            Self::Chromatic => &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11],
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Major => "Major",
            Self::Minor => "Minor",
            Self::HarmonicMinor => "Harmonic Minor",
            Self::MelodicMinor => "Melodic Minor",
            Self::Dorian => "Dorian",
            Self::Phrygian => "Phrygian",
            Self::Lydian => "Lydian",
            Self::Mixolydian => "Mixolydian",
            Self::Locrian => "Locrian",
            Self::MajorPentatonic => "Major Pentatonic",
            Self::MinorPentatonic => "Minor Pentatonic",
            Self::Blues => "Blues",
            Self::Chromatic => "Chromatic",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Scale {
    pub root: Key,
    pub kind: ScaleKind,
}

impl Scale {
    pub fn new(root: Key, kind: ScaleKind) -> Self {
        Self { root, kind }
    }

    /// 12-bit mask of the pitch classes in the scale (bit 0 = C).
    pub fn mask(self) -> u16 {
        self.kind
            .intervals()
            .iter()
            .fold(0u16, |m, i| m | 1 << ((self.root.0 + i) % 12))
    }

    pub fn contains(self, midi_note: u8) -> bool {
        self.mask() & (1 << (midi_note % 12)) != 0
    }

    /// Nearest in-scale note, preferring downward on ties.
    pub fn snap(self, midi_note: u8) -> u8 {
        (0..=6)
            .flat_map(|d: i16| [-d, d])
            .map(|d| midi_note as i16 + d)
            .find(|&n| (0..=127).contains(&n) && self.contains(n as u8))
            .unwrap_or(midi_note as i16) as u8
    }

    /// Scale degree (0-based) → MIDI note, starting from `octave` (C4 = 60 means octave 4).
    pub fn degree_to_note(self, degree: i32, octave: i32) -> u8 {
        let iv = self.kind.intervals();
        let n = iv.len() as i32;
        let oct = octave + degree.div_euclid(n);
        let pc = self.root.0 as i32 + iv[degree.rem_euclid(n) as usize] as i32;
        ((oct + 1) * 12 + pc).clamp(0, 127) as u8
    }

    /// Camelot wheel code (e.g. "8A" for A minor), used by DJs and the sample browser.
    pub fn camelot(self) -> Option<String> {
        let minor = matches!(self.kind, ScaleKind::Minor | ScaleKind::Dorian | ScaleKind::Phrygian | ScaleKind::HarmonicMinor | ScaleKind::MelodicMinor | ScaleKind::MinorPentatonic | ScaleKind::Blues);
        let major = matches!(self.kind, ScaleKind::Major | ScaleKind::Lydian | ScaleKind::Mixolydian | ScaleKind::MajorPentatonic);
        // Position on the wheel: majors start at B=1B, minors at G#m=1A, each step a fifth.
        let base = if minor { 8 } else if major { 11 } else { return None };
        let steps = ((self.root.0 as i32 - base).rem_euclid(12) * 7).rem_euclid(12);
        Some(format!("{}{}", steps + 1, if minor { 'A' } else { 'B' }))
    }

    pub fn display(self) -> String {
        format!("{} {}", self.root.name(), self.kind.label())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_minor() {
        let s = Scale::new(Key::from_name("A").unwrap(), ScaleKind::Minor);
        assert!(s.contains(69) && s.contains(60) && !s.contains(61));
        assert_eq!(s.snap(61), 60);
        assert_eq!(s.degree_to_note(0, 3), 57);
        assert_eq!(s.camelot().as_deref(), Some("8A"));
    }

    #[test]
    fn camelot_majors() {
        let c = Scale::new(Key(0), ScaleKind::Major);
        assert_eq!(c.camelot().as_deref(), Some("8B"));
        let g = Scale::new(Key(7), ScaleKind::Major);
        assert_eq!(g.camelot().as_deref(), Some("9B"));
    }

    #[test]
    fn key_names() {
        assert_eq!(Key::from_name("Bb"), Some(Key(10)));
        assert_eq!(Key::from_name("f#"), Some(Key(6)));
        assert_eq!(Key(1).name(), "C#");
    }
}
