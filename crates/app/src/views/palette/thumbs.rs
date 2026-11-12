//! Plugin thumbnails for device results: a cached screenshot of the plugin's
//! editor (`<config>/thumbnails/plugins/<id>.png`) when one exists, looked
//! up on a background thread; otherwise the row draws a generated card
//! (vendor colour, category icon, vendor initials).
//!
//! [`Covers`] does the same for sample rows: the sample pack's artwork
//! ([`aitrack_library::cover`]), resolved once per folder on a background
//! thread; rows without one keep the pack-initials tile.

use crossbeam_channel::{Receiver, Sender, unbounded};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub fn dir() -> PathBuf {
    crate::appearance::config_dir().join("thumbnails").join("plugins")
}

/// File-name-safe key for a plugin id (stock id or VST3/CLAP uid).
pub fn key(id: &str) -> String {
    id.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '.' { c } else { '_' }).collect()
}

/// Up to two initials of a vendor or plugin name ("Fab Filter" → "FF").
pub fn initials(name: &str) -> String {
    let words: Vec<&str> = name.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    match words.as_slice() {
        [] => "?".into(),
        [one] => one.chars().take(2).collect::<String>().to_uppercase(),
        [a, b, ..] => format!("{}{}", a.chars().next().unwrap_or('?'), b.chars().next().unwrap_or('?')).to_uppercase(),
    }
}

/// Stable colour slot for a name.
pub fn hue_slot(name: &str) -> usize {
    name.bytes().fold(7usize, |h, b| h.wrapping_mul(31).wrapping_add(b as usize)) % crate::theme::SERIES
}

pub struct Thumbs {
    found: HashMap<String, Option<PathBuf>>,
    tx: Sender<String>,
    rx: Receiver<(String, Option<PathBuf>)>,
}

impl Default for Thumbs {
    fn default() -> Self {
        let (tx, jobs) = unbounded::<String>();
        let (done, rx) = unbounded();
        let run = move || {
            let dir = dir();
            while let Ok(id) = jobs.recv() {
                let k = key(&id);
                let hit = ["png", "jpg"].iter().map(|e| dir.join(format!("{k}.{e}"))).find(|p| p.is_file());
                if done.send((id, hit)).is_err() {
                    break;
                }
            }
        };
        let _ = std::thread::Builder::new().name("palette-thumbs".into()).spawn(run);
        Self { found: HashMap::new(), tx, rx }
    }
}

impl Thumbs {
    /// The cached screenshot of `id`, once looked up.
    pub fn get(&mut self, id: &str) -> Option<PathBuf> {
        match self.found.get(id) {
            Some(p) => p.clone(),
            None => {
                self.found.insert(id.to_owned(), None);
                let _ = self.tx.send(id.to_owned());
                None
            }
        }
    }

    pub fn poll(&mut self) -> bool {
        let mut any = false;
        while let Ok((id, hit)) = self.rx.try_recv() {
            any |= hit.is_some();
            self.found.insert(id, hit);
        }
        any
    }
}

/// Sample pack covers, cached per sample folder (`None` = looked up, none found).
pub struct Covers {
    found: HashMap<PathBuf, Option<PathBuf>>,
    tx: Sender<(PathBuf, PathBuf, Option<String>)>,
    rx: Receiver<(PathBuf, Option<PathBuf>)>,
}

impl Default for Covers {
    fn default() -> Self {
        let (tx, jobs) = unbounded::<(PathBuf, PathBuf, Option<String>)>();
        let (done, rx) = unbounded();
        let run = move || {
            while let Ok((dir, sample, pack)) = jobs.recv() {
                let hit = aitrack_library::cover::sample_cover(&sample, pack.as_deref());
                if done.send((dir, hit)).is_err() {
                    break;
                }
            }
        };
        let _ = std::thread::Builder::new().name("palette-covers".into()).spawn(run);
        Self { found: HashMap::new(), tx, rx }
    }
}

impl Covers {
    /// The cover for `sample` once its folder has been looked up; the first
    /// ask queues the lookup and answers `None`.
    pub fn get(&mut self, sample: &Path, pack: Option<&str>) -> Option<PathBuf> {
        let dir = sample.parent()?;
        if let Some(hit) = self.found.get(dir) {
            return hit.clone();
        }
        self.found.insert(dir.to_owned(), None);
        let _ = self.tx.send((dir.to_owned(), sample.to_owned(), pack.map(str::to_owned)));
        None
    }

    /// Take finished lookups; `true` when a cover turned up.
    pub fn poll(&mut self) -> bool {
        let mut any = false;
        while let Ok((dir, hit)) = self.rx.try_recv() {
            any |= hit.is_some();
            self.found.insert(dir, hit);
        }
        any
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initials_keys_and_colours() {
        assert_eq!(initials("Fab Filter"), "FF");
        assert_eq!(initials("u-he"), "UH");
        assert_eq!(initials("Surge"), "SU");
        assert_eq!(initials(""), "?");
        assert_eq!(key("com.x/Y z"), "com.x_Y_z");
        assert_eq!(hue_slot("Arturia"), hue_slot("Arturia"));
    }

    #[test]
    fn covers_resolve_once_per_folder_in_the_background() {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let tmp = std::env::temp_dir().join(format!("aitrack-covers-{}-{nanos}", std::process::id()));
        let pack = tmp.join("Deep Pack");
        std::fs::create_dir_all(pack.join("Kicks")).unwrap();
        std::fs::write(pack.join("cover.jpg"), b"x").unwrap();
        let (a, b) = (pack.join("Kicks/a.wav"), pack.join("Kicks/b.wav"));
        let mut c = Covers::default();
        assert_eq!(c.get(&a, Some("Deep Pack")), None, "queued");
        assert_eq!(c.get(&b, Some("Deep Pack")), None, "same folder, not queued twice");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !c.poll() {
            assert!(std::time::Instant::now() < deadline, "lookup never finished");
            std::thread::yield_now();
        }
        assert_eq!(c.get(&b, Some("Deep Pack")), Some(pack.join("cover.jpg")));
        assert_eq!(c.found.len(), 1);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
