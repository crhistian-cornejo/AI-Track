//! Finding the folder the Splice desktop app saves the user's licensed
//! sounds to. Nothing here talks to Splice: it only looks at local folders.
//!
//! Splice documents `~/Splice` as the place (Preferences let the user move
//! it) and the Ableton integration writes to `User Library/Samples/Splice`.
//! The app's own layout keeps downloads under `Splice/sounds`; that
//! subfolder is not in Splice's help pages, so it is tried first and the
//! documented folder next. A folder holding audio wins over an empty one.

use std::path::{Path, PathBuf};

/// Where a Splice folder was found and what it holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Detected {
    pub folder: PathBuf,
    /// Audio files were seen in it (a fresh install has none yet).
    pub has_audio: bool,
}

/// The places to look under `home`, best guess first.
pub fn candidates_under(home: &Path) -> Vec<PathBuf> {
    let user_library = |base: &str| home.join(base).join("Ableton").join("User Library").join("Samples").join("Splice");
    vec![home.join("Splice").join("sounds"), home.join("Splice"), user_library("Music"), user_library("Documents")]
}

/// The first of `candidates` that holds audio, else the first folder that exists.
pub fn detect_among(candidates: &[PathBuf]) -> Option<Detected> {
    let mut empty = None;
    for c in candidates.iter().filter(|c| c.is_dir()) {
        if holds_audio(c) {
            return Some(Detected { folder: c.clone(), has_audio: true });
        }
        empty.get_or_insert_with(|| Detected { folder: c.clone(), has_audio: false });
    }
    empty
}

/// The Splice folder of this user, when there is one.
pub fn detect() -> Option<Detected> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    detect_among(&candidates_under(Path::new(&home)))
}

/// The default place to name when nothing was found (the first candidate).
pub fn default_folder() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    candidates_under(Path::new(&home)).into_iter().next()
}

/// At least one audio file within a few levels (a bounded look: the folder can be huge).
fn holds_audio(dir: &Path) -> bool {
    let mut seen = 0usize;
    walkdir::WalkDir::new(dir)
        .max_depth(8)
        .into_iter()
        .filter_map(|e| e.ok())
        .take_while(|_| {
            seen += 1;
            seen < 5000
        })
        .any(|e| e.file_type().is_file() && crate::scan::is_audio(e.path()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_with_sounds_beats_an_empty_one_and_nothing_is_none() {
        let home = tempfile::tempdir().unwrap();
        let c = candidates_under(home.path());
        assert_eq!(detect_among(&c), None);

        std::fs::create_dir_all(&c[0]).unwrap();
        assert_eq!(detect_among(&c), Some(Detected { folder: c[0].clone(), has_audio: false }), "installed, nothing licensed yet");

        let pack = c[1].join("packs").join("Deep House");
        std::fs::create_dir_all(&pack).unwrap();
        std::fs::write(pack.join("kick_128bpm.wav"), b"RIFF").unwrap();
        // c[1] (~/Splice) holds audio but c[0] (~/Splice/sounds) is inside it and empty: the one with audio.
        let d = detect_among(&c).unwrap();
        assert!(d.has_audio);
        assert_eq!(d.folder, c[1]);
    }

    #[test]
    fn the_ableton_user_library_is_a_candidate() {
        let home = tempfile::tempdir().unwrap();
        let c = candidates_under(home.path());
        let abl = c.iter().find(|p| p.ends_with("Samples/Splice")).unwrap().clone();
        std::fs::create_dir_all(&abl).unwrap();
        std::fs::write(abl.join("Snare.flac"), b"fLaC").unwrap();
        assert_eq!(detect_among(&c), Some(Detected { folder: abl, has_audio: true }));
    }
}
