//! User plugin preferences persisted under the config folder:
//! custom scan folders, palette category overrides, favorites and recent plugins.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

/// Persisted plugin preferences (`plugin_prefs.json`).
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct PluginPrefs {
    /// Additional user folders scanned alongside OS VST3/CLAP defaults.
    pub folders: Vec<PathBuf>,
    /// Stable plugin keys starred as favorites (`stock:<id>` or `ext:<format>:<uid>`).
    pub favorites: Vec<String>,
    /// Recently inserted plugin keys (most recent first).
    pub recents: Vec<String>,
    /// User-reassigned device group (`plugin_key -> group_slug`).
    pub category_overrides: BTreeMap<String, String>,
}

const MAX_RECENTS: usize = 16;

fn path() -> PathBuf {
    crate::appearance::config_dir().join("plugin_prefs.json")
}

impl PluginPrefs {
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

    pub fn add_folder(&mut self, dir: PathBuf) -> bool {
        if self.folders.iter().any(|p| p == &dir) {
            return false;
        }
        self.folders.push(dir);
        self.save();
        true
    }

    pub fn remove_folder(&mut self, dir: &Path) -> bool {
        let before = self.folders.len();
        self.folders.retain(|p| p != dir);
        if self.folders.len() != before {
            self.save();
            true
        } else {
            false
        }
    }

    pub fn is_favorite(&self, key: &str) -> bool {
        self.favorites.iter().any(|k| k == key)
    }

    pub fn toggle_favorite(&mut self, key: &str) -> bool {
        if let Some(i) = self.favorites.iter().position(|k| k == key) {
            self.favorites.remove(i);
            self.save();
            false
        } else {
            self.favorites.push(key.to_owned());
            self.save();
            true
        }
    }

    pub fn favorite_set(&self) -> HashSet<&str> {
        self.favorites.iter().map(String::as_str).collect()
    }

    pub fn note_recent(&mut self, key: &str) {
        self.recents.retain(|k| k != key);
        self.recents.insert(0, key.to_owned());
        self.recents.truncate(MAX_RECENTS);
        self.save();
    }

    pub fn set_category_override(&mut self, key: &str, group: Option<&str>) {
        match group {
            Some(g) => {
                self.category_overrides.insert(key.to_owned(), g.to_owned());
            }
            None => {
                self.category_overrides.remove(key);
            }
        }
        self.save();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_favorites_and_recents_dedupe() {
        let mut prefs = PluginPrefs::default();
        assert!(!prefs.folders.iter().any(|p| p == Path::new("/tmp/vst3")));
        prefs.folders.push(PathBuf::from("/tmp/vst3"));
        prefs.favorites.push("stock:fx.eq".into());
        assert!(prefs.is_favorite("stock:fx.eq"));

        prefs.recents.push("a".into());
        prefs.recents.retain(|k| k != "b");
        prefs.recents.insert(0, "b".into());
        prefs.recents.retain(|k| k != "a");
        prefs.recents.insert(0, "a".into());
        assert_eq!(prefs.recents, vec!["a", "b"]);
    }
}
