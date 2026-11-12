//! The palette's saved options (`palette.json` in the config folder).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PaletteSettings {
    /// Search Splice too (its unofficial API). Off until the user opts in.
    pub splice: bool,
    /// The folder the Splice desktop app saves licensed sounds to, indexed
    /// into the library. `None` = Splice's default (`~/Splice/sounds`).
    pub splice_folder: Option<PathBuf>,
}

fn path() -> PathBuf {
    crate::appearance::config_dir().join("palette.json")
}

impl PaletteSettings {
    pub fn load() -> Self {
        std::fs::read(path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        let p = path();
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(p, json);
        }
    }

    pub fn splice_folder(&self) -> Option<PathBuf> {
        self.splice_folder.clone().or_else(aitrack_splice::default_sounds_folder)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splice_is_off_by_default_and_old_files_load() {
        assert!(!PaletteSettings::default().splice);
        let s: PaletteSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(s, PaletteSettings::default());
        let s: PaletteSettings = serde_json::from_str(r#"{"splice":true}"#).unwrap();
        assert!(s.splice);
    }
}
