//! The sample taxonomy: categories, subcategories, sample kinds and the
//! loop families shown in the virtual folder tree.

use serde::{Deserialize, Serialize};

/// Top-level sound category. Stored in SQLite as [`Category::as_str`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Category {
    Drums,
    Bass,
    Synths,
    Vocals,
    Fx,
    Instruments,
}

impl Category {
    pub const ALL: [Category; 6] = [
        Self::Drums,
        Self::Bass,
        Self::Synths,
        Self::Vocals,
        Self::Fx,
        Self::Instruments,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Drums => "Drums",
            Self::Bass => "Bass",
            Self::Synths => "Synths",
            Self::Vocals => "Vocals",
            Self::Fx => "FX",
            Self::Instruments => "Instruments",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|c| c.as_str().eq_ignore_ascii_case(s.trim()))
    }

    /// Canonical subcategories, in display order.
    pub fn subcategories(self) -> &'static [&'static str] {
        match self {
            Self::Drums => &["Kicks", "Snares", "Claps", "Hats", "Percs", "Cymbals", "808s"],
            Self::Bass => &[],
            Self::Synths => &["Leads", "Pads", "Plucks", "Keys", "Arps", "Chords"],
            Self::Vocals => &[],
            Self::Fx => &["Risers", "Impacts", "Sweeps", "Atmos"],
            Self::Instruments => &[
                "Piano", "Strings", "Brass", "Woodwinds", "Mallets", "Guitar", "Choir", "Organ",
            ],
        }
    }

    /// Whether samples of this category usually carry a musical key.
    pub fn is_tonal(self) -> bool {
        !matches!(self, Self::Drums | Self::Fx)
    }
}

/// One-shot, loop or chop.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum SampleKind {
    #[default]
    OneShot,
    Loop,
    Chop,
}

impl SampleKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OneShot => "one_shot",
            Self::Loop => "loop",
            Self::Chop => "chop",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "one_shot" => Some(Self::OneShot),
            "loop" => Some(Self::Loop),
            "chop" => Some(Self::Chop),
            _ => None,
        }
    }
}

/// Loop family for the `Loops/…` virtual folders.
pub const LOOP_FAMILIES: [&str; 5] = ["Drum", "Melodic", "Bass", "Vocal", "Top"];

/// Derives the loop family from the sound's category, subcategory and the
/// "top loop" hint (a drum loop without kick).
pub fn loop_family(category: Option<Category>, sub: Option<&str>, top_hint: bool) -> &'static str {
    match category {
        Some(Category::Drums) => {
            if top_hint || matches!(sub, Some("Hats" | "Percs" | "Cymbals")) {
                "Top"
            } else {
                "Drum"
            }
        }
        Some(Category::Bass) => "Bass",
        Some(Category::Vocals) => "Vocal",
        Some(Category::Synths | Category::Instruments) => "Melodic",
        Some(Category::Fx) | None => {
            if top_hint {
                "Top"
            } else {
                "Drum"
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        for c in Category::ALL {
            assert_eq!(Category::parse(c.as_str()), Some(c));
        }
        assert_eq!(Category::parse("fx"), Some(Category::Fx));
        for k in [SampleKind::OneShot, SampleKind::Loop, SampleKind::Chop] {
            assert_eq!(SampleKind::parse(k.as_str()), Some(k));
        }
    }

    #[test]
    fn families() {
        assert_eq!(loop_family(Some(Category::Drums), Some("Hats"), false), "Top");
        assert_eq!(loop_family(Some(Category::Drums), Some("Kicks"), false), "Drum");
        assert_eq!(loop_family(Some(Category::Synths), None, false), "Melodic");
    }
}
