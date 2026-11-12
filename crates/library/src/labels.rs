//! A static catalogue of popular sample labels and stores, grouped by the
//! genres they are known for. Each entry links to the label's official site
//! and, when it has one, to its official free-packs page (a page, never a
//! file). The palette's Packs tab lists them as chips filterable by genre.

/// Genres the catalogue groups labels by.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Genre {
    House,
    TechHouse,
    Techno,
    Trance,
    Edm,
    BigRoom,
    DrumAndBass,
    Bass,
    Dubstep,
    Trap,
    HipHop,
    LoFi,
    Pop,
}

impl Genre {
    pub const ALL: &'static [Genre] = &[
        Genre::House,
        Genre::TechHouse,
        Genre::Techno,
        Genre::Trance,
        Genre::Edm,
        Genre::BigRoom,
        Genre::DrumAndBass,
        Genre::Bass,
        Genre::Dubstep,
        Genre::Trap,
        Genre::HipHop,
        Genre::LoFi,
        Genre::Pop,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Genre::House => "House",
            Genre::TechHouse => "Tech house",
            Genre::Techno => "Techno",
            Genre::Trance => "Trance",
            Genre::Edm => "EDM",
            Genre::BigRoom => "Big room",
            Genre::DrumAndBass => "Drum & bass",
            Genre::Bass => "Bass music",
            Genre::Dubstep => "Dubstep",
            Genre::Trap => "Trap",
            Genre::HipHop => "Hip hop",
            Genre::LoFi => "Lo-fi",
            Genre::Pop => "Pop",
        }
    }
}

/// A sample label or store.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Label {
    pub name: &'static str,
    /// The official site (https).
    pub url: &'static str,
    pub genres: &'static [Genre],
    /// The label's official free-packs page, if it has one.
    pub free_url: Option<&'static str>,
}

impl Label {
    pub fn has(&self, g: Genre) -> bool {
        self.genres.contains(&g)
    }
}

use Genre::*;

pub const LABELS: &[Label] = &[
    Label { name: "Splice", url: "https://splice.com/sounds", genres: &[House, Techno, Edm, Trap, HipHop, LoFi, Pop], free_url: None },
    Label { name: "Loopmasters", url: "https://www.loopmasters.com", genres: &[House, TechHouse, Techno, DrumAndBass, Bass], free_url: None },
    Label { name: "Vengeance", url: "https://www.vengeance-sound.com", genres: &[Edm, House, Trance], free_url: None },
    Label {
        name: "Black Octopus Sound",
        url: "https://blackoctopus-sound.com",
        genres: &[Bass, DrumAndBass, Techno, House],
        free_url: Some("https://blackoctopus-sound.com/free-downloads/"),
    },
    Label { name: "Cymatics", url: "https://cymatics.fm", genres: &[Trap, HipHop, Edm, LoFi], free_url: Some("https://cymatics.fm/pages/free-download-vault") },
    Label { name: "Production Master", url: "https://production-master.com", genres: &[House, Techno, HipHop], free_url: None },
    Label { name: "Sample Magic", url: "https://www.samplemagic.com", genres: &[House, Techno, Pop], free_url: None },
    Label { name: "Ghosthack", url: "https://www.ghosthack.de", genres: &[Edm, Trap, Dubstep], free_url: Some("https://www.ghosthack.de/free_sample_packs/") },
    Label {
        name: "Function Loops",
        url: "https://www.functionloops.com",
        genres: &[Edm, House, Trance],
        free_url: Some("https://www.functionloops.com/free-samples.html"),
    },
    Label { name: "Toolroom Academy", url: "https://toolroomacademy.com", genres: &[House, TechHouse], free_url: None },
    Label { name: "Riemann Kollektion", url: "https://riemannkollektion.com", genres: &[Techno], free_url: None },
    Label { name: "Prime Loops", url: "https://primeloops.com", genres: &[HipHop, Trap, Edm], free_url: Some("https://primeloops.com/collections/freebies-taster-packs") },
    Label { name: "Samplephonics", url: "https://www.samplephonics.com", genres: &[House, Techno, DrumAndBass], free_url: None },
    Label { name: "Dharma Worldwide", url: "https://www.dharmaworldwide.com", genres: &[BigRoom, Edm], free_url: None },
];

/// Labels known for `genre` (every label for `None`), in catalogue order.
pub fn by_genre(genre: Option<Genre>) -> impl Iterator<Item = &'static Label> {
    LABELS.iter().filter(move |l| genre.is_none_or(|g| l.has(g)))
}

/// Genres at least one label is listed under, in [`Genre::ALL`] order.
pub fn genres_in_use() -> Vec<Genre> {
    Genre::ALL.iter().copied().filter(|g| LABELS.iter().any(|l| l.has(*g))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn urls_are_https_and_unique() {
        let mut names = HashSet::new();
        let mut urls = HashSet::new();
        for l in LABELS {
            assert!(names.insert(l.name.to_lowercase()), "duplicate label {}", l.name);
            assert!(l.url.starts_with("https://"), "{}: {}", l.name, l.url);
            assert!(urls.insert(l.url), "duplicate url {}", l.url);
            if let Some(f) = l.free_url {
                assert!(f.starts_with("https://"), "{}: {f}", l.name);
                assert!(urls.insert(f), "duplicate url {f}");
                // A page, never a direct download.
                let lower = f.to_lowercase();
                for ext in [".zip", ".rar", ".7z", ".wav", ".tar", ".gz"] {
                    assert!(!lower.ends_with(ext), "{}: {f} looks like a file", l.name);
                }
            }
            assert!(!l.genres.is_empty(), "{} has no genre", l.name);
        }
    }

    #[test]
    fn genre_filter() {
        assert_eq!(by_genre(None).count(), LABELS.len());
        assert!(by_genre(Some(Genre::Techno)).any(|l| l.name == "Riemann Kollektion"));
        assert!(by_genre(Some(Genre::Techno)).all(|l| l.has(Genre::Techno)));
        assert!(by_genre(Some(Genre::BigRoom)).any(|l| l.name == "Dharma Worldwide"));
        let used = genres_in_use();
        assert!(used.contains(&Genre::House));
        assert!(used.windows(2).all(|w| w[0] < w[1]));
    }
}
